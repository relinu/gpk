#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "../filters.h"
#include "../lzrc.h"
int main(int c, char**v){ FILE*f=fopen(v[1],"rb");fseek(f,0,SEEK_END);size_t n=ftell(f);fseek(f,0,SEEK_SET);
 unsigned char*a=malloc(n),*b=malloc(n),*d=malloc(n),*z=malloc(lzrc_bound(n)); if(fread(a,1,n,f)!=n)return 1;
 fspec fs={0}; fs.id=FT_BCJ; filter_encode(&fs,a,b,n,0); memcpy(z,b,n); filter_decode(&fs,z,d,n,0);
 size_t i=0; while(i<n&&a[i]==d[i])i++; printf("BCJ roundtrip: %s (erste Abweichung %zu)\n", i==n?"OK":"FEHLER", i);
 lzrc_params p; lzrc_default_params(&p,9); size_t cs=lzrc_compress(a,n,z,lzrc_bound(n),&p);
 int ok=lzrc_decompress(z,cs,d,n); printf("LZRC roh: %s\n", ok&&!memcmp(a,d,n)?"OK":"FEHLER");
 cs=lzrc_compress(b,n,z,lzrc_bound(n),&p); ok=lzrc_decompress(z,cs,d,n); printf("LZRC bcj: %s\n", ok&&!memcmp(b,d,n)?"OK":"FEHLER");}
