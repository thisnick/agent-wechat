#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include "SKP_Silk_SDK_API.h"

/* Small explicit FFI boundary; the caller owns all buffers and validates packet
 * framing before decode. One supported profile: mono16kHz/20ms/16kbps/complexity2. */
int aw_silk_encode(const int16_t *pcm,int samples,uint8_t *out,int capacity,int *length){
 if(!pcm||!out||!length||samples<1||samples>800000||capacity<10)return -1;
 int32_t state_size=0;if(SKP_Silk_SDK_Get_Encoder_Size(&state_size)||state_size<1||state_size>1048576)return -2;
 void *state=calloc(1,state_size);if(!state)return -3;
 SKP_SILK_SDK_EncControlStruct control;
 if(SKP_Silk_SDK_InitEncoder(state,&control)){free(state);return -4;}
 control.API_sampleRate=16000;control.maxInternalSampleRate=16000;control.packetSize=320;control.bitRate=16000;control.packetLossPercentage=0;control.complexity=2;control.useInBandFEC=0;control.useDTX=0;
 out[0]=2;memcpy(out+1,"#!SILK_V3",9);int at=10;
 for(int pos=0;pos<samples;pos+=320){int16_t frame[320]={0};int n=samples-pos;if(n>320)n=320;memcpy(frame,pcm+pos,n*sizeof(int16_t));uint8_t packet[1024];int16_t bytes=1024;int status=SKP_Silk_SDK_Encode(state,&control,frame,320,packet,&bytes);if(status||bytes<1||bytes>1024||at+2+bytes>capacity){free(state);return -5;}out[at++]=(uint8_t)bytes;out[at++]=(uint8_t)(bytes>>8);memcpy(out+at,packet,bytes);at+=bytes;}
 memset(state,0,state_size);free(state);*length=at;return 0;
}
int aw_silk_decode(const uint8_t *input,int length,int16_t *pcm,int capacity,int *samples){
 if(!input||!pcm||!samples||length<12||capacity<320||input[0]>3||memcmp(input+1,"#!SILK_V3",9))return -1;
 int32_t state_size=0;if(SKP_Silk_SDK_Get_Decoder_Size(&state_size)||state_size<1||state_size>1048576)return -2;
 void *state=calloc(1,state_size);if(!state)return -3;if(SKP_Silk_SDK_InitDecoder(state)){free(state);return -4;}
 SKP_SILK_SDK_DecControlStruct control={0};control.API_sampleRate=16000;int at=10,total=0,packets=0;
 while(at<length){if(at+2>length){free(state);return -5;}int bytes=input[at]|(input[at+1]<<8);at+=2;if(bytes<1||bytes>5120||bytes>length-at||++packets>3000){free(state);return -6;}int frames=0;do{int16_t output[480];int16_t count=480;int status=SKP_Silk_SDK_Decode(state,&control,0,input+at,bytes,output,&count);if(status||count<1||count>480||total+count>capacity||++frames>5){free(state);return -7;}memcpy(pcm+total,output,count*sizeof(int16_t));total+=count;}while(control.moreInternalDecoderFrames);at+=bytes;}
 memset(state,0,state_size);free(state);*samples=total;return 0;
}
