#include <stdint.h>
#include <stdlib.h>
#include <dlfcn.h>
/* Narrow dynamic ABI: no Python, no codec headers or copied library structs.
 * FDK's stream-info prefix is sampleRate/frameSize/numChannels (three INTs). */
struct Audio {
    void *fdk,*pa,*decoder,*stream;
    void *(*open)(int,unsigned); unsigned (*config)(void*,uint8_t**,unsigned*);
    unsigned (*fill)(void*,uint8_t**,unsigned*,unsigned*);
    unsigned (*decode)(void*,int16_t*,int,unsigned); int *(*info)(void*);void (*close)(void*);
    int (*init)(void),(*terminate)(void),(*start)(void*),(*stop)(void*),(*free_stream)(void*);
    int (*new_stream)(void**,int,int,unsigned long,double,unsigned long,void*,void*);
    int (*write)(void*,const void*,unsigned long);
    int16_t pcm[8192],output[16384];int initialized;
};
void iss_audio_free(void *ptr){struct Audio*a=ptr;if(!a)return;if(a->stream){a->stop(a->stream);a->free_stream(a->stream);}if(a->initialized)a->terminate();if(a->decoder)a->close(a->decoder);if(a->fdk)dlclose(a->fdk);if(a->pa)dlclose(a->pa);free(a);}
#define LOAD(lib,field,name) do{*(void**)(&a->field)=dlsym(a->lib,name);if(!a->field)goto fail;}while(0)
void *iss_audio_new(void){
    struct Audio*a=calloc(1,sizeof(*a));if(!a)return NULL;
    a->fdk=dlopen("libfdk-aac.so.2",RTLD_NOW|RTLD_LOCAL);a->pa=dlopen("libportaudio.so.2",RTLD_NOW|RTLD_LOCAL);if(!a->fdk||!a->pa)goto fail;
    LOAD(fdk,open,"aacDecoder_Open");LOAD(fdk,config,"aacDecoder_ConfigRaw");LOAD(fdk,fill,"aacDecoder_Fill");LOAD(fdk,decode,"aacDecoder_DecodeFrame");LOAD(fdk,info,"aacDecoder_GetStreamInfo");LOAD(fdk,close,"aacDecoder_Close");
    LOAD(pa,init,"Pa_Initialize");LOAD(pa,terminate,"Pa_Terminate");LOAD(pa,new_stream,"Pa_OpenDefaultStream");LOAD(pa,start,"Pa_StartStream");LOAD(pa,stop,"Pa_AbortStream");LOAD(pa,free_stream,"Pa_CloseStream");LOAD(pa,write,"Pa_WriteStream");
    a->decoder=a->open(0,1);if(!a->decoder)goto fail;
    uint8_t asc[]={0xf8,0xe6,0x51,0x32,0xe0,0};uint8_t*p=asc;unsigned size=6;if(a->config(a->decoder,&p,&size))goto fail;
    return a;
fail:iss_audio_free(a);return NULL;
}
int iss_audio_decode_pcm(void *ptr,const uint8_t *data,unsigned size,int16_t *out,unsigned capacity){
    struct Audio*a=ptr;uint8_t*p=(uint8_t*)data;unsigned valid=size;
    if(a->fill(a->decoder,&p,&size,&valid)||a->decode(a->decoder,a->pcm,8192,0))return 0;
    int*info=a->info(a->decoder);if(!info)return 0;
    int rate=info[0],frames=info[1],channels=info[2];
    if((rate!=48000&&rate!=24000)||(channels!=1&&channels!=2)||frames<=0||frames*channels>8192)return -1;
    int factor=48000/rate;int count=frames*factor;if(count*2>16384)return -1;
    for(int f=0;f<count;f++)for(int c=0;c<2;c++)a->output[f*2+c]=a->pcm[(f/factor)*channels+(channels==1?0:c)];
    if(out){if(capacity<(unsigned)count*2)return -1;for(int i=0;i<count*2;i++)out[i]=a->output[i];}
    return count;
}
int iss_audio_feed(void *ptr,const uint8_t *data,unsigned size){
    struct Audio*a=ptr;int count=iss_audio_decode_pcm(ptr,data,size,NULL,0);if(count<=0)return count;
    if(!a->stream){if(a->init())return -1;a->initialized=1;if(a->new_stream(&a->stream,0,2,8,48000,480,NULL,NULL)||a->start(a->stream))return -1;}
    int error=a->write(a->stream,a->output,(unsigned long)count);
    return error==0||error==-9980?count:-1; /* output underflow can recover */
}
