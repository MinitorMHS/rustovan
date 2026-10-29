// OpenCL C PBKDF2-HMAC-SHA512 kernel. One work-item derives one BIP39 seed.
// OpenCL ulong expresses SHA-512's 64-bit operations directly.
__constant ulong K[80] = {
  0x428a2f98d728ae22UL,0x7137449123ef65cdUL,0xb5c0fbcfec4d3b2fUL,0xe9b5dba58189dbbcUL,0x3956c25bf348b538UL,0x59f111f1b605d019UL,0x923f82a4af194f9bUL,0xab1c5ed5da6d8118UL,
  0xd807aa98a3030242UL,0x12835b0145706fbeUL,0x243185be4ee4b28cUL,0x550c7dc3d5ffb4e2UL,0x72be5d74f27b896fUL,0x80deb1fe3b1696b1UL,0x9bdc06a725c71235UL,0xc19bf174cf692694UL,
  0xe49b69c19ef14ad2UL,0xefbe4786384f25e3UL,0x0fc19dc68b8cd5b5UL,0x240ca1cc77ac9c65UL,0x2de92c6f592b0275UL,0x4a7484aa6ea6e483UL,0x5cb0a9dcbd41fbd4UL,0x76f988da831153b5UL,
  0x983e5152ee66dfabUL,0xa831c66d2db43210UL,0xb00327c898fb213fUL,0xbf597fc7beef0ee4UL,0xc6e00bf33da88fc2UL,0xd5a79147930aa725UL,0x06ca6351e003826fUL,0x142929670a0e6e70UL,
  0x27b70a8546d22ffcUL,0x2e1b21385c26c926UL,0x4d2c6dfc5ac42aedUL,0x53380d139d95b3dfUL,0x650a73548baf63deUL,0x766a0abb3c77b2a8UL,0x81c2c92e47edaee6UL,0x92722c851482353bUL,
  0xa2bfe8a14cf10364UL,0xa81a664bbc423001UL,0xc24b8b70d0f89791UL,0xc76c51a30654be30UL,0xd192e819d6ef5218UL,0xd69906245565a910UL,0xf40e35855771202aUL,0x106aa07032bbd1b8UL,
  0x19a4c116b8d2d0c8UL,0x1e376c085141ab53UL,0x2748774cdf8eeb99UL,0x34b0bcb5e19b48a8UL,0x391c0cb3c5c95a63UL,0x4ed8aa4ae3418acbUL,0x5b9cca4f7763e373UL,0x682e6ff3d6b2b8a3UL,
  0x748f82ee5defb2fcUL,0x78a5636f43172f60UL,0x84c87814a1f0ab72UL,0x8cc702081a6439ecUL,0x90befffa23631e28UL,0xa4506cebde82bde9UL,0xbef9a3f7b2c67915UL,0xc67178f2e372532bUL,
  0xca273eceea26619cUL,0xd186b8c721c0c207UL,0xeada7dd6cde0eb1eUL,0xf57d4f7fee6ed178UL,0x06f067aa72176fbaUL,0x0a637dc5a2c898a6UL,0x113f9804bef90daeUL,0x1b710b35131c471bUL,
  0x28db77f523047d84UL,0x32caab7b40c72493UL,0x3c9ebe0a15c9bebcUL,0x431d67c49c100d4cUL,0x4cc5d4becb3e42b6UL,0x597f299cfc657e2aUL,0x5fcb6fab3ad6faecUL,0x6c44198c4a475817UL
};
__constant ulong IV[8] = {
  0x6a09e667f3bcc908UL,0xbb67ae8584caa73bUL,0x3c6ef372fe94f82bUL,0xa54ff53a5f1d36f1UL,
  0x510e527fade682d1UL,0x9b05688c2b3e6c1fUL,0x1f83d9abfb41bd6bUL,0x5be0cd19137e2179UL
};
inline ulong ror(ulong x,uint n){return (x>>n)|(x<<(64u-n));}
inline ulong S0(ulong x){return ror(x,28)^ror(x,34)^ror(x,39);}
inline ulong S1(ulong x){return ror(x,14)^ror(x,18)^ror(x,41);}
inline ulong s0(ulong x){return ror(x,1)^ror(x,8)^(x>>7);}
inline ulong s1(ulong x){return ror(x,19)^ror(x,61)^(x>>6);}
inline ulong load64(__private const uchar *p){ulong x=0;for(uint i=0;i<8;i++)x=(x<<8)|(ulong)p[i];return x;}
inline void store64(__private uchar *p,ulong x){for(uint i=0;i<8;i++)p[i]=(uchar)(x>>(56u-8u*i));}
inline void compress_words(__private ulong *h,__private ulong *w){
  ulong a=h[0],b=h[1],c=h[2],d=h[3],e=h[4],f=h[5],g=h[6],z=h[7];
  for(uint i=0;i<80;i++){uint j=i&15u;if(i>=16)w[j]=s1(w[(i-2)&15u])+w[(i-7)&15u]+s0(w[(i-15)&15u])+w[j];
    ulong x=z+S1(e)+((e&f)^(~e&g))+K[i]+w[j],y=S0(a)+((a&b)^(a&c)^(b&c));
    z=g;g=f;f=e;e=d+x;d=c;c=b;b=a;a=x+y;}
  h[0]+=a;h[1]+=b;h[2]+=c;h[3]+=d;h[4]+=e;h[5]+=f;h[6]+=g;h[7]+=z;
}
inline void compress(__private ulong *h,__private const uchar *block){
  ulong w[16];for(uint i=0;i<16;i++)w[i]=load64(block+i*8u);
  compress_words(h,w);
}
inline void hmac64(__private const ulong *ik,__private const ulong *ok,
                   __private const ulong *message,__private ulong *out){
  ulong inner[8],outer[8],w[16];
  for(uint i=0;i<8;i++){inner[i]=ik[i];w[i]=message[i];}
  w[8]=0x8000000000000000UL;for(uint i=9;i<15;i++)w[i]=0;w[15]=1536UL;
  compress_words(inner,w);
  for(uint i=0;i<8;i++){outer[i]=ok[i];w[i]=inner[i];}
  w[8]=0x8000000000000000UL;for(uint i=9;i<15;i++)w[i]=0;w[15]=1536UL;
  compress_words(outer,w);
  for(uint i=0;i<8;i++)out[i]=outer[i];
}
inline void hmac(__private const ulong *ik,__private const ulong *ok,__private const uchar *msg,uint n,__private ulong *out){
  ulong inner[8],outer[8];uchar b[128];
  for(uint i=0;i<8;i++)inner[i]=ik[i];
  for(uint i=0;i<128;i++)b[i]=0;
  for(uint i=0;i<n;i++)b[i]=msg[i];
  b[n]=0x80;ulong bits=(128UL+(ulong)n)*8UL;
  for(uint i=0;i<8;i++)b[127-i]=(uchar)(bits>>(8*i));
  compress(inner,b);
  for(uint i=0;i<8;i++)outer[i]=ok[i];
  for(uint i=0;i<128;i++)b[i]=0;
  for(uint i=0;i<8;i++)store64(b+8*i,inner[i]);
  b[64]=0x80;bits=1536UL;
  for(uint i=0;i<8;i++)b[127-i]=(uchar)(bits>>(8*i));
  compress(outer,b);
  for(uint i=0;i<8;i++)out[i]=outer[i];
}
__kernel void pbkdf2_sha512(__global const uchar *passwords,__global const uint *lengths,
                            __global const uchar *salt,uint salt_len,uint count,__global uchar *output){
  uint index=(uint)get_global_id(0);if(index>=count)return;
  ulong ik[8],ok[8];for(uint i=0;i<8;i++){ik[i]=IV[i];ok[i]=IV[i];}
  uchar b[128];uint plen=lengths[index];
  for(uint i=0;i<128;i++)b[i]=((i<plen)?passwords[index*128u+i]:(uchar)0)^(uchar)0x36;
  compress(ik,b);
  for(uint i=0;i<128;i++)b[i]=((i<plen)?passwords[index*128u+i]:(uchar)0)^(uchar)0x5c;
  compress(ok,b);
  uchar first[128];for(uint i=0;i<128;i++)first[i]=0;for(uint i=0;i<salt_len;i++)first[i]=salt[i];first[salt_len+3u]=1;
  ulong u[8],t[8];hmac(ik,ok,first,salt_len+4u,u);for(uint i=0;i<8;i++)t[i]=u[i];
  for(uint round=1;round<2048;round++){ulong next[8];hmac64(ik,ok,u,next);for(uint i=0;i<8;i++){u[i]=next[i];t[i]^=next[i];}}
  for(uint i=0;i<8;i++)for(uint j=0;j<8;j++)output[index*64u+8u*i+j]=(uchar)(t[i]>>(56u-8u*j));
}


