#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <lzma.h>
#include "../lzrc.h"
static double now(void){struct timespec t;clock_gettime(CLOCK_MONOTONIC,&t);return t.tv_sec+t.tv_nsec*1e-9;}
int main(int argc,char**argv){
  for(int a=1;a<argc;a++){
    FILE*f=fopen(argv[a],"rb");fseek(f,0,SEEK_END);size_t n=ftell(f);fseek(f,0,SEEK_SET);
    unsigned char*s=malloc(n),*c=malloc(lzrc_bound(n)),*d=malloc(n+1);
    if(fread(s,1,n,f)!=n)return 1;fclose(f);
    lzrc_params p;lzrc_default_params(&p,9);
    double t0=now();size_t cs=lzrc_compress(s,n,c,lzrc_bound(n),&p);double tc=now()-t0;
    t0=now();int ok=lzrc_decompress(c,cs,d,n);double td=now()-t0;
    ok=ok&&!memcmp(s,d,n);
    size_t xs=0;unsigned char*x=malloc(n+n/2+1024);
    lzma_easy_buffer_encode(9|LZMA_PRESET_EXTREME,LZMA_CHECK_NONE,NULL,s,n,x,&xs,n+n/2+1024);
    printf("%-26s %9zu  lzrc %9zu (%5.2fx, c %5.1f MB/s, d %6.1f MB/s) %s | xz-9e %9zu (%5.2fx)  lzrc/xz %.3f\n",
      strrchr(argv[a],'/')+1,n,cs,(double)n/cs,n/tc/1e6,n/td/1e6,ok?"OK":"FEHLER",xs,(double)n/xs,(double)cs/xs);
  }
}
