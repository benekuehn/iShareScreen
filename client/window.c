#define _POSIX_C_SOURCE 200809L
#define GLFW_INCLUDE_NONE
#include <GLFW/glfw3.h>
#define GLFW_EXPOSE_NATIVE_WAYLAND
#include <GLFW/glfw3native.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
/* Callbacks and queue belong exclusively to the window/event thread. */
struct Event { int kind, a, b, c; double x, y; };
struct Window { GLFWwindow *window; GLFWcursor *cursor; struct Event events[512]; unsigned head,count; int overflow; };
static void push(GLFWwindow *w, struct Event e) {
    struct Window *s=glfwGetWindowUserPointer(w);
    if(s->count==512){s->overflow=1; return;}
    s->events[(s->head+s->count++)%512]=e;
}
static void motion(GLFWwindow*w,double x,double y){push(w,(struct Event){.kind=1,.x=x,.y=y});}
static void button(GLFWwindow*w,int b,int a,int m){push(w,(struct Event){.kind=2,.a=b,.b=a,.c=m});}
static void scroll(GLFWwindow*w,double x,double y){push(w,(struct Event){.kind=3,.x=x,.y=y});}
static void key(GLFWwindow*w,int k,int s,int a,int m){(void)s;push(w,(struct Event){.kind=4,.a=k,.b=a,.c=m});}
static void text(GLFWwindow*w,unsigned cp){push(w,(struct Event){.kind=5,.a=(int)cp});}
static void focus(GLFWwindow*w,int f){push(w,(struct Event){.kind=6,.a=f});}
void *iss_window_new(void) {
    if(!glfwInit())return NULL;
    glfwWindowHint(GLFW_CLIENT_API,GLFW_OPENGL_ES_API);
    glfwWindowHint(GLFW_CONTEXT_VERSION_MAJOR,3);
    glfwWindowHint(GLFW_CONTEXT_VERSION_MINOR,0);
    glfwWindowHint(GLFW_CONTEXT_CREATION_API,GLFW_EGL_CONTEXT_API);
    struct Window *s=calloc(1,sizeof(*s));
    if(!s){glfwTerminate();return NULL;}
    s->window=glfwCreateWindow(1280,720,"iShareScreen — Rust GPU",NULL,NULL);
    if(!s->window){free(s);glfwTerminate();return NULL;}
    glfwSetWindowUserPointer(s->window,s);
    glfwSetCursorPosCallback(s->window,motion);glfwSetMouseButtonCallback(s->window,button);
    glfwSetScrollCallback(s->window,scroll);glfwSetKeyCallback(s->window,key);
    glfwSetCharCallback(s->window,text);glfwSetWindowFocusCallback(s->window,focus);
    return s;
}
void iss_window_context(void *p,int acquire){glfwMakeContextCurrent(acquire?((struct Window*)p)->window:NULL);if(acquire)glfwSwapInterval(0);}
void iss_window_swap(void *p){glfwSwapBuffers(((struct Window*)p)->window);}
int iss_window_state(void *p,int *out) {
    struct Window*s=p;glfwGetFramebufferSize(s->window,out,out+1);
    out[2]=glfwGetWindowAttrib(s->window,GLFW_FOCUSED);
    out[3]=!glfwGetWindowAttrib(s->window,GLFW_ICONIFIED);
    glfwGetWindowSize(s->window,out+4,out+5);
    return glfwWindowShouldClose(s->window)||s->overflow;
}
void iss_window_wait(double seconds){glfwWaitEventsTimeout(seconds);}
void iss_window_wake(void){glfwPostEmptyEvent();}
double iss_process_cpu_seconds(void){struct timespec t;if(clock_gettime(CLOCK_PROCESS_CPUTIME_ID,&t))return 0;return t.tv_sec+t.tv_nsec/1e9;}
void *iss_window_display(void){return glfwGetPlatform()==GLFW_PLATFORM_WAYLAND?glfwGetWaylandDisplay():NULL;}
void *iss_window_surface(void*p){return glfwGetPlatform()==GLFW_PLATFORM_WAYLAND?glfwGetWaylandWindow(((struct Window*)p)->window):NULL;}
int iss_window_event(void *p,struct Event *e){struct Window*s=p;if(!s->count)return 0;*e=s->events[s->head];s->head=(s->head+1)%512;s->count--;return 1;}
void iss_window_cursor(void*p,int w,int h,int x,int y,uint8_t*rgba){struct Window*s=p;GLFWcursor*c=NULL;if(rgba){GLFWimage image={w,h,rgba};c=glfwCreateCursor(&image,x,y);if(!c)return;}glfwSetCursor(s->window,c);if(s->cursor)glfwDestroyCursor(s->cursor);s->cursor=c;}
const char *iss_window_clipboard(void*p){return glfwGetClipboardString(((struct Window*)p)->window);}
void iss_window_set_clipboard(void*p,const char*text){glfwSetClipboardString(((struct Window*)p)->window,text);}
void iss_window_free(void *p){struct Window*s=p;if(s->cursor)glfwDestroyCursor(s->cursor);glfwDestroyWindow(s->window);free(s);glfwTerminate();}
