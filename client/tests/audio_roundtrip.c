/* Optional offline integration check: synthetic PCM -> FDK AAC-ELD-SBR ->
 * the actual client decoder. No sound device is opened, no host is contacted.
 * cc client/tests/audio_roundtrip.c client/audio.c -lfdk-aac -ldl -lm -o /tmp/iss-audio-check
 * /tmp/iss-audio-check */
#include <fdk-aac/aacenc_lib.h>
#include <stdint.h>
#include <stdio.h>
#include <math.h>
#include <stdlib.h>
extern void *iss_audio_new(void);
extern void iss_audio_free(void*);
extern int iss_audio_decode_pcm(void*,const uint8_t*,unsigned,int16_t*,unsigned);
#define CHECK(call) do{int e=(call);if(e){fprintf(stderr,"%s: 0x%x\n",#call,e);return 1;}}while(0)
int main(void){
    HANDLE_AACENCODER encoder=NULL;CHECK(aacEncOpen(&encoder,0,2));
    CHECK(aacEncoder_SetParam(encoder,AACENC_AOT,39));
    CHECK(aacEncoder_SetParam(encoder,AACENC_SAMPLERATE,48000));
    CHECK(aacEncoder_SetParam(encoder,AACENC_CHANNELMODE,MODE_2));
    /* This tuning selects the SBR frequency table used by Apple's cookie. */
    CHECK(aacEncoder_SetParam(encoder,AACENC_BITRATE,96000));
    CHECK(aacEncoder_SetParam(encoder,AACENC_SBR_MODE,1));
    CHECK(aacEncoder_SetParam(encoder,AACENC_GRANULE_LENGTH,480));
    CHECK(aacEncoder_SetParam(encoder,AACENC_TRANSMUX,0));
    CHECK(aacEncEncode(encoder,NULL,NULL,NULL,NULL));AACENC_InfoStruct info;CHECK(aacEncInfo(encoder,&info));
    if(info.confSize<6||info.confBuf[3]!=0x32)return 1;
    void *decoder=iss_audio_new();if(!decoder)return 1;
    INT_PCM pcm[4096];uint8_t encoded[8192];int16_t decoded[16384];long total=0,measured=0;int peak=0;double real=0,imaginary=0;
    for(int frame=0;frame<40;frame++){
        if(info.frameLength*2>4096)return 1;
        for(unsigned i=0;i<info.frameLength;i++){INT_PCM sample=(INT_PCM)(6000*sin(2*3.141592653589793*440*(frame*info.frameLength+i)/48000.));pcm[i*2]=sample;pcm[i*2+1]=sample;}
        void *input=pcm,*output=encoded;INT in_id=IN_AUDIO_DATA,out_id=OUT_BITSTREAM_DATA,in_size=info.frameLength*2*sizeof(INT_PCM),out_size=sizeof(encoded),in_element=sizeof(INT_PCM),out_element=1;
        AACENC_BufDesc in={1,&input,&in_id,&in_size,&in_element},out={1,&output,&out_id,&out_size,&out_element};
        AACENC_InArgs args={0};args.numInSamples=info.frameLength*2;AACENC_OutArgs result={0};CHECK(aacEncEncode(encoder,&in,&out,&args,&result));
        if(result.numOutBytes){int n=iss_audio_decode_pcm(decoder,encoded,result.numOutBytes,decoded,16384);if(n<0)return 1;for(int i=0;i<n;i++){int value=abs(decoded[i*2]);if(value>peak)peak=value;if(total+i>2000){double phase=2*3.141592653589793*440*(total+i)/48000.;real+=decoded[i*2]*cos(phase);imaginary+=decoded[i*2]*sin(phase);measured++;}}total+=n;}
    }
    iss_audio_free(decoder);aacEncClose(&encoder);
    double amplitude=measured?2*hypot(real,imaginary)/measured:0;
    if(total<15000||peak>12000||amplitude<2000||amplitude>9000)return 1;
    printf("Offline AAC-ELD-SBR roundtrip: %ld PCM frames, 440 Hz amplitude=%.0f, peak=%d; no playback device opened\n",total,amplitude,peak);return 0;
}
