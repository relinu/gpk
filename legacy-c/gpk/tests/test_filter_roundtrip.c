#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "../filters.h"
int main(void){ srand(1); int fails=0, runs=0;
 static unsigned char a[300000],b[300000],c[300000],d[300000];
 for(int it=0;it<20000;it++){
  size_t n=rand()%sizeof a; unsigned long long off=(rand()%4==0)?0:(unsigned long long)rand()*7;
  for(size_t i=0;i<n;i++) a[i]= (rand()%3==0)? (unsigned char)(0xE8|(rand()&1)) : (rand()%2? 0xFF: rand());
  fspec f; memset(&f,0,sizeof f); f.id=rand()%FT_COUNT; f.stride=1+rand()%32;
  if(f.id==FT_WPRED){ int W=(rand()%2)?2:4; f.fields[0]=W; f.fields[1]=1+rand()%2; f.fields[2]=rand()%2; f.stride=W*(1+rand()%16); }
  if(f.id==FT_SPLIT){ f.nfields=1+rand()%6; for(int k=0;k<f.nfields;k++) f.fields[k]=1+rand()%8; f.skip=rand()%300; }
  filter_encode(&f,a,b,n,off); memcpy(c,b,n); filter_decode(&f,c,d,n,off);
  runs++; if(memcmp(a,d,n)){ fails++; if(fails<5) printf("FEHLER filter=%s n=%zu off=%llu\n",FILTER_NAMES[f.id],n,off);} }
 printf("%d Filter-Roundtrips, %d Fehler\n",runs,fails); return fails!=0; }
