#define _POSIX_C_SOURCE 200809L
#include <errno.h>
#include <stdint.h>
#include <limits.h>
#include <math.h>
#include <stdlib.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>
#include <libavcodec/avcodec.h>
#include <libavutil/hwcontext.h>
#include <libavutil/hwcontext_vaapi.h>
#include <libavutil/pixdesc.h>
#include <va/va.h>
#include <va/va_drmcommon.h>
#include <EGL/egl.h>
#include <EGL/eglext.h>
#include <GLES3/gl3.h>
#include <GLES2/gl2ext.h>

static _Thread_local char last_error[1024];
const char *bridge_error(void) { return last_error; }
static int fail(const char *where, int err) {
    char text[256]; av_strerror(err, text, sizeof(text));
    snprintf(last_error, sizeof(last_error), "%s: %s (%d)", where, text, err);
    return -1;
}
static int glfail(const char *where) {
    snprintf(last_error, sizeof(last_error), "%s: EGL=0x%x GL=0x%x", where, eglGetError(), glGetError());
    return -1;
}
static enum AVPixelFormat choose_vaapi(AVCodecContext *ctx, const enum AVPixelFormat *formats) {
    (void)ctx;
    for (const enum AVPixelFormat *p = formats; *p != AV_PIX_FMT_NONE; ++p)
        if (*p == AV_PIX_FMT_VAAPI) return *p;
    return AV_PIX_FMT_NONE; /* Hardware-only: never silently decode on CPU. */
}
void *bridge_decoder_new(int codec_id, const uint8_t *extra, size_t len, const char *device) {
    const AVCodec *codec = avcodec_find_decoder(codec_id == 0 ? AV_CODEC_ID_H264 : AV_CODEC_ID_HEVC);
    if (!codec || len > INT_MAX) { fail("codec/extradata", AVERROR(EINVAL)); return NULL; }
    AVCodecContext *ctx = avcodec_alloc_context3(codec);
    if (!ctx) { fail("decoder allocation", AVERROR(ENOMEM)); return NULL; }
    int ret = av_hwdevice_ctx_create(&ctx->hw_device_ctx, AV_HWDEVICE_TYPE_VAAPI, device, NULL, 0);
    if (ret < 0) { fail("VAAPI device", ret); avcodec_free_context(&ctx); return NULL; }
    ctx->get_format = choose_vaapi;
    ctx->thread_count = 1;
    ctx->flags |= AV_CODEC_FLAG_LOW_DELAY;
    ctx->pkt_timebase = (AVRational){1, 90000};
    if (len) {
        ctx->extradata = av_mallocz(len + AV_INPUT_BUFFER_PADDING_SIZE);
        if (!ctx->extradata) { fail("extradata allocation", AVERROR(ENOMEM)); avcodec_free_context(&ctx); return NULL; }
        memcpy(ctx->extradata, extra, len); ctx->extradata_size = (int)len;
    }
    ret = avcodec_open2(ctx, codec, NULL);
    if (ret < 0) { fail("decoder open", ret); avcodec_free_context(&ctx); return NULL; }
    return ctx;
}
void bridge_decoder_free(void *ptr) { AVCodecContext *ctx = ptr; avcodec_free_context(&ctx); }
void bridge_decoder_flush(void *ptr) { avcodec_flush_buffers(ptr); }
int bridge_decoder_drain(void *ptr) {
    int ret=avcodec_send_packet(ptr,NULL);
    return ret<0 && ret!=AVERROR_EOF ? fail("decoder drain",ret):0;
}
int bridge_decoder_send(void *ptr, const uint8_t *data, size_t len, int64_t pts) {
    if (len > INT_MAX) return fail("packet too large", AVERROR(EINVAL));
    AVPacket *pkt = av_packet_alloc();
    if (!pkt) return fail("packet allocation", AVERROR(ENOMEM));
    int ret = av_new_packet(pkt, (int)len);
    if (ret >= 0) {
        memcpy(pkt->data, data, len); pkt->pts = pkt->dts = pts;
        ret = avcodec_send_packet(ptr, pkt);
    }
    av_packet_free(&pkt);
    if (ret == AVERROR(EAGAIN)) return 1;
    return ret < 0 ? fail("decoder send", ret) : 0;
}
void *bridge_decoder_receive(void *ptr) {
    last_error[0] = 0;
    AVFrame *frame = av_frame_alloc();
    if (!frame) { fail("frame allocation", AVERROR(ENOMEM)); return NULL; }
    int ret = avcodec_receive_frame(ptr, frame);
    if (ret < 0) {
        if (ret != AVERROR(EAGAIN) && ret != AVERROR_EOF) fail("decoder receive", ret);
        av_frame_free(&frame); return NULL;
    }
    if (frame->format != AV_PIX_FMT_VAAPI) { fail("non-VAAPI output", AVERROR(ENOSYS)); av_frame_free(&frame); return NULL; }
    return frame;
}
void bridge_frame_free(void *ptr) { AVFrame *frame = ptr; av_frame_free(&frame); }
int bridge_frame_wait(void *ptr) {
    AVFrame *frame=ptr;
    const AVHWFramesContext *hw=(const AVHWFramesContext*)frame->hw_frames_ctx->data;
    const AVVAAPIDeviceContext *va=hw->device_ctx->hwctx;
    VAStatus status=vaSyncSurface(va->display,(VASurfaceID)(uintptr_t)frame->data[3]);
    if(status!=VA_STATUS_SUCCESS){
        snprintf(last_error,sizeof(last_error),"decode completion: %s",vaErrorStr(status));
        return -1;
    }
    return 0;
}
int bridge_frame_ready(void *ptr) {
    AVFrame *frame=ptr;
    const AVHWFramesContext *hw=(const AVHWFramesContext*)frame->hw_frames_ctx->data;
    const AVVAAPIDeviceContext *va=hw->device_ctx->hwctx;
    VASurfaceStatus state;
    VAStatus status=vaQuerySurfaceStatus(va->display,(VASurfaceID)(uintptr_t)frame->data[3],&state);
    /* An unsupported query falls back to the synchronized upload path. */
    return status!=VA_STATUS_SUCCESS ? -1 : state==VASurfaceReady;
}
struct FrameInfo { int64_t pts; int width,height,error,full_range,matrix,chroma444; };
void bridge_frame_info(void *ptr, struct FrameInfo *info) {
    const AVFrame *frame = ptr;
    const AVHWFramesContext *hw = (const AVHWFramesContext *)frame->hw_frames_ctx->data;
    const AVPixFmtDescriptor *fmt = av_pix_fmt_desc_get(hw->sw_format);
    *info = (struct FrameInfo){frame->pts,frame->width,frame->height,
        frame->decode_error_flags | ((frame->flags & AV_FRAME_FLAG_CORRUPT) ? 1 : 0),
        frame->color_range != AVCOL_RANGE_MPEG,frame->colorspace,
        fmt && !fmt->log2_chroma_w && !fmt->log2_chroma_h};
}

/* GPU composition copies into a GPU canvas, never into CPU image buffers.
 * Each imported decoder surface is pinned until its fence signals. */
struct Lease { AVFrame *frame; EGLImage image; GLuint texture; GLsync fence; };
struct Renderer {
    EGLDisplay display;
    GLuint canvas, fbo, external_program, present_program, vao;
    int width,height;
    struct Lease leases[16];
    unsigned count;
};
static PFNEGLCREATEIMAGEKHRPROC create_image;
static PFNEGLDESTROYIMAGEKHRPROC destroy_image;
static PFNGLEGLIMAGETARGETTEXTURE2DOESPROC image_target;

static GLuint shader(GLenum type, const char *text) {
    GLuint s = glCreateShader(type); glShaderSource(s, 1, &text, NULL); glCompileShader(s);
    GLint ok; glGetShaderiv(s, GL_COMPILE_STATUS, &ok);
    if (!ok) { glGetShaderInfoLog(s,sizeof(last_error),NULL,last_error); glDeleteShader(s); return 0; }
    return s;
}
static GLuint program(const char *fragment, int flip) {
    const char *vs_normal = "#version 300 es\nout vec2 uv;void main(){vec2 p=vec2((gl_VertexID<<1)&2,gl_VertexID&2);uv=p;gl_Position=vec4(p*2.-1.,0.,1.);}";
    const char *vs_flip = "#version 300 es\nout vec2 uv;uniform vec2 crop;void main(){vec2 p=vec2((gl_VertexID<<1)&2,gl_VertexID&2);uv=vec2(p.x,1.-p.y)*crop;gl_Position=vec4(p*2.-1.,0.,1.);}";
    GLuint vs=shader(GL_VERTEX_SHADER,flip?vs_flip:vs_normal), fs=shader(GL_FRAGMENT_SHADER,fragment);
    if (!vs || !fs) { if(vs)glDeleteShader(vs);if(fs)glDeleteShader(fs);return 0; }
    GLuint p=glCreateProgram(); glAttachShader(p,vs);glAttachShader(p,fs);glLinkProgram(p);glDeleteShader(vs);glDeleteShader(fs);
    GLint ok;glGetProgramiv(p,GL_LINK_STATUS,&ok);
    if(!ok){glGetProgramInfoLog(p,sizeof(last_error),NULL,last_error);glDeleteProgram(p);return 0;}return p;
}
static void release_lease(struct Renderer *r, struct Lease *l) {
    if(l->fence)glDeleteSync(l->fence);
    glDeleteTextures(1,&l->texture);destroy_image(r->display,l->image);av_frame_free(&l->frame);
}
static void reap(struct Renderer *r, int wait) {
    /* GL_TIMEOUT_IGNORED is for glWaitSync, not glClientWaitSync. A failed
     * client wait must never leave a full ring that we then write past. */
    if(wait) {
        glFinish();
        for(unsigned i=0;i<r->count;i++)release_lease(r,&r->leases[i]);
        r->count=0;
        return;
    }
    unsigned keep=0;
    for(unsigned i=0;i<r->count;i++) {
        struct Lease l=r->leases[i];
        GLenum result=l.fence?glClientWaitSync(l.fence,0,0):GL_WAIT_FAILED;
        if(result==GL_ALREADY_SIGNALED || result==GL_CONDITION_SATISFIED)release_lease(r,&l);
        else r->leases[keep++]=l;
    }r->count=keep;
}
void bridge_renderer_free(void *ptr) {
    struct Renderer *r=ptr;if(!r)return;
    reap(r,1);
    glDeleteProgram(r->external_program);glDeleteProgram(r->present_program);
    glDeleteTextures(1,&r->canvas);glDeleteFramebuffers(1,&r->fbo);glDeleteVertexArrays(1,&r->vao);free(r);
}
void *bridge_renderer_new(int w,int h) {
    EGLDisplay display=eglGetCurrentDisplay();
    if(display==EGL_NO_DISPLAY){glfail("no current EGL context");return NULL;}
    const char *extensions=eglQueryString(display,EGL_EXTENSIONS);
    if(!extensions || !strstr(extensions,"EGL_EXT_image_dma_buf_import")){glfail("DMA-BUF import unavailable");return NULL;}
    create_image=(PFNEGLCREATEIMAGEKHRPROC)eglGetProcAddress("eglCreateImageKHR");
    destroy_image=(PFNEGLDESTROYIMAGEKHRPROC)eglGetProcAddress("eglDestroyImageKHR");
    image_target=(PFNGLEGLIMAGETARGETTEXTURE2DOESPROC)eglGetProcAddress("glEGLImageTargetTexture2DOES");
    if(!create_image||!destroy_image||!image_target){glfail("EGL image entrypoints");return NULL;}
    struct Renderer *r=calloc(1,sizeof(*r));if(!r){fail("renderer allocation",AVERROR(ENOMEM));return NULL;}
    r->display=display;r->width=w;r->height=h;
    const char *ext_fs="#version 300 es\n#extension GL_OES_EGL_image_external_essl3 : require\nprecision highp float;in vec2 uv;uniform samplerExternalOES image;out vec4 color;void main(){color=texture(image,uv);}";
    const char *present_fs="#version 300 es\nprecision highp float;in vec2 uv;uniform sampler2D image;out vec4 color;void main(){color=texture(image,uv);}";
    r->external_program=program(ext_fs,1);r->present_program=program(present_fs,0);
    if(!r->external_program||!r->present_program){bridge_renderer_free(r);return NULL;}
    glGenVertexArrays(1,&r->vao);glBindVertexArray(r->vao);
    glGenTextures(1,&r->canvas);glBindTexture(GL_TEXTURE_2D,r->canvas);
    glTexStorage2D(GL_TEXTURE_2D,1,GL_RGBA8,w,h);
    glTexParameteri(GL_TEXTURE_2D,GL_TEXTURE_MIN_FILTER,GL_LINEAR);glTexParameteri(GL_TEXTURE_2D,GL_TEXTURE_MAG_FILTER,GL_LINEAR);
    glTexParameteri(GL_TEXTURE_2D,GL_TEXTURE_WRAP_S,GL_CLAMP_TO_EDGE);glTexParameteri(GL_TEXTURE_2D,GL_TEXTURE_WRAP_T,GL_CLAMP_TO_EDGE);
    glGenFramebuffers(1,&r->fbo);glBindFramebuffer(GL_FRAMEBUFFER,r->fbo);
    glFramebufferTexture2D(GL_FRAMEBUFFER,GL_COLOR_ATTACHMENT0,GL_TEXTURE_2D,r->canvas,0);
    if(glCheckFramebufferStatus(GL_FRAMEBUFFER)!=GL_FRAMEBUFFER_COMPLETE){glfail("canvas framebuffer");bridge_renderer_free(r);return NULL;}
    glClearColor(0,0,0,1);glClear(GL_COLOR_BUFFER_BIT);glBindFramebuffer(GL_FRAMEBUFFER,0);
    if(glGetError()!=GL_NO_ERROR){glfail("canvas setup");bridge_renderer_free(r);return NULL;}return r;
}
int bridge_renderer_upload(void *ptr, void *frame_ptr, int origin, int rows) {
    struct Renderer *r=ptr;AVFrame *frame=frame_ptr;
    reap(r,0);if(r->count>=16)reap(r,1);
    const AVHWFramesContext *hw=(const AVHWFramesContext*)frame->hw_frames_ctx->data;
    const AVVAAPIDeviceContext *va=hw->device_ctx->hwctx;
    VASurfaceID surface=(VASurfaceID)(uintptr_t)frame->data[3];
    VAStatus status=vaSyncSurface(va->display,surface);
    if(status!=VA_STATUS_SUCCESS){snprintf(last_error,sizeof(last_error),"vaSyncSurface: %s",vaErrorStr(status));return -1;}
    VADRMPRIMESurfaceDescriptor desc={0};
    status=vaExportSurfaceHandle(va->display,surface,VA_SURFACE_ATTRIB_MEM_TYPE_DRM_PRIME_2,VA_EXPORT_SURFACE_READ_ONLY|VA_EXPORT_SURFACE_COMPOSED_LAYERS,&desc);
    if(status!=VA_STATUS_SUCCESS){snprintf(last_error,sizeof(last_error),"vaExportSurfaceHandle: %s",vaErrorStr(status));return -1;}
    if(desc.num_layers!=1 || desc.layers[0].num_planes>3){
        for(uint32_t i=0;i<desc.num_objects;i++)close(desc.objects[i].fd);
        snprintf(last_error,sizeof(last_error),"unsupported DRM surface layout (%u layers)",desc.num_layers);return -1;
    }
    EGLint attrs[64];int n=0;
    #define ATTR(k,v) do{attrs[n++]=(k);attrs[n++]=(EGLint)(v);}while(0)
    ATTR(EGL_WIDTH,desc.width);ATTR(EGL_HEIGHT,desc.height);ATTR(EGL_LINUX_DRM_FOURCC_EXT,desc.layers[0].drm_format);
    const EGLint fds[]={EGL_DMA_BUF_PLANE0_FD_EXT,EGL_DMA_BUF_PLANE1_FD_EXT,EGL_DMA_BUF_PLANE2_FD_EXT};
    const EGLint offsets[]={EGL_DMA_BUF_PLANE0_OFFSET_EXT,EGL_DMA_BUF_PLANE1_OFFSET_EXT,EGL_DMA_BUF_PLANE2_OFFSET_EXT};
    const EGLint pitches[]={EGL_DMA_BUF_PLANE0_PITCH_EXT,EGL_DMA_BUF_PLANE1_PITCH_EXT,EGL_DMA_BUF_PLANE2_PITCH_EXT};
    const EGLint lows[]={EGL_DMA_BUF_PLANE0_MODIFIER_LO_EXT,EGL_DMA_BUF_PLANE1_MODIFIER_LO_EXT,EGL_DMA_BUF_PLANE2_MODIFIER_LO_EXT};
    const EGLint highs[]={EGL_DMA_BUF_PLANE0_MODIFIER_HI_EXT,EGL_DMA_BUF_PLANE1_MODIFIER_HI_EXT,EGL_DMA_BUF_PLANE2_MODIFIER_HI_EXT};
    for(uint32_t p=0;p<desc.layers[0].num_planes;p++) {
        uint32_t obj=desc.layers[0].object_index[p];
        ATTR(fds[p],desc.objects[obj].fd);ATTR(offsets[p],desc.layers[0].offset[p]);ATTR(pitches[p],desc.layers[0].pitch[p]);
        ATTR(lows[p],desc.objects[obj].drm_format_modifier&0xffffffff);ATTR(highs[p],desc.objects[obj].drm_format_modifier>>32);
    }
    ATTR(EGL_YUV_COLOR_SPACE_HINT_EXT,frame->colorspace==AVCOL_SPC_BT470BG||frame->colorspace==AVCOL_SPC_SMPTE170M?EGL_ITU_REC601_EXT:EGL_ITU_REC709_EXT);
    ATTR(EGL_SAMPLE_RANGE_HINT_EXT,frame->color_range==AVCOL_RANGE_MPEG?EGL_YUV_NARROW_RANGE_EXT:EGL_YUV_FULL_RANGE_EXT);
    attrs[n++]=EGL_NONE;
    EGLImage image=create_image(r->display,EGL_NO_CONTEXT,EGL_LINUX_DMA_BUF_EXT,NULL,attrs);
    for(uint32_t i=0;i<desc.num_objects;i++)close(desc.objects[i].fd);
    if(image==EGL_NO_IMAGE_KHR){glfail("EGL DMA-BUF import");return -1;}
    AVFrame *pin=av_frame_clone(frame);
    if(!pin){destroy_image(r->display,image);return fail("surface lease",AVERROR(ENOMEM));}
    GLuint tex;glGenTextures(1,&tex);glActiveTexture(GL_TEXTURE0);glBindTexture(GL_TEXTURE_EXTERNAL_OES,tex);
    glTexParameteri(GL_TEXTURE_EXTERNAL_OES,GL_TEXTURE_MIN_FILTER,GL_LINEAR);glTexParameteri(GL_TEXTURE_EXTERNAL_OES,GL_TEXTURE_MAG_FILTER,GL_LINEAR);
    glTexParameteri(GL_TEXTURE_EXTERNAL_OES,GL_TEXTURE_WRAP_S,GL_CLAMP_TO_EDGE);glTexParameteri(GL_TEXTURE_EXTERNAL_OES,GL_TEXTURE_WRAP_T,GL_CLAMP_TO_EDGE);
    image_target(GL_TEXTURE_EXTERNAL_OES,image);
    glBindFramebuffer(GL_FRAMEBUFFER,r->fbo);glViewport(0,r->height-origin-rows,frame->width,rows);
    glUseProgram(r->external_program);glUniform1i(glGetUniformLocation(r->external_program,"image"),0);
    glUniform2f(glGetUniformLocation(r->external_program,"crop"),(float)frame->width/desc.width,(float)rows/desc.height);
    glBindVertexArray(r->vao);glDrawArrays(GL_TRIANGLES,0,3);
    GLsync fence=glFenceSync(GL_SYNC_GPU_COMMANDS_COMPLETE,0);glFlush();
    if(!fence){
        glFinish();glDeleteTextures(1,&tex);destroy_image(r->display,image);av_frame_free(&pin);
        glBindFramebuffer(GL_FRAMEBUFFER,0);return glfail("GPU composition fence");
    }
    r->leases[r->count++]=(struct Lease){pin,image,tex,fence};
    glBindFramebuffer(GL_FRAMEBUFFER,0);
    if(glGetError()!=GL_NO_ERROR)return glfail("GPU tile composition");
    return 0;
}
int bridge_renderer_draw(void *ptr,int w,int h) {
    struct Renderer *r=ptr;reap(r,0);glBindFramebuffer(GL_FRAMEBUFFER,0);glViewport(0,0,w,h);
    glClearColor(0,0,0,1);glClear(GL_COLOR_BUFFER_BIT);
    float fit=fminf((float)w/r->width,(float)h/r->height);
    int dw=(int)(r->width*fit),dh=(int)(r->height*fit);glViewport((w-dw)/2,(h-dh)/2,dw,dh);
    glUseProgram(r->present_program);glActiveTexture(GL_TEXTURE0);glBindTexture(GL_TEXTURE_2D,r->canvas);
    glUniform1i(glGetUniformLocation(r->present_program,"image"),0);glBindVertexArray(r->vao);glDrawArrays(GL_TRIANGLES,0,3);
    return glGetError()!=GL_NO_ERROR?glfail("GPU presentation"):0;
}
