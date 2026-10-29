#include <cuda_runtime.h>
#include <stdint.h>
#include <string.h>
#include <stdlib.h>

#define CUDA_OK(x) do { cudaError_t e=(x); if(e!=cudaSuccess) return (int)e; } while(0)

__device__ __forceinline__ uint64_t rotr(uint64_t x, int n){ return (x>>n)|(x<<(64-n)); }
__device__ __forceinline__ uint64_t Ch(uint64_t x,uint64_t y,uint64_t z){return (x&y)^(~x&z);}
__device__ __forceinline__ uint64_t Maj(uint64_t x,uint64_t y,uint64_t z){return (x&y)^(x&z)^(y&z);}
__device__ __forceinline__ uint64_t BS0(uint64_t x){return rotr(x,28)^rotr(x,34)^rotr(x,39);}
__device__ __forceinline__ uint64_t BS1(uint64_t x){return rotr(x,14)^rotr(x,18)^rotr(x,41);}
__device__ __forceinline__ uint64_t SS0(uint64_t x){return rotr(x,1)^rotr(x,8)^(x>>7);}
__device__ __forceinline__ uint64_t SS1(uint64_t x){return rotr(x,19)^rotr(x,61)^(x>>6);}

__device__ const uint64_t K[80]={
0x428a2f98d728ae22ULL,0x7137449123ef65cdULL,0xb5c0fbcfec4d3b2fULL,0xe9b5dba58189dbbcULL,
0x3956c25bf348b538ULL,0x59f111f1b605d019ULL,0x923f82a4af194f9bULL,0xab1c5ed5da6d8118ULL,
0xd807aa98a3030242ULL,0x12835b0145706fbeULL,0x243185be4ee4b28cULL,0x550c7dc3d5ffb4e2ULL,
0x72be5d74f27b896fULL,0x80deb1fe3b1696b1ULL,0x9bdc06a725c71235ULL,0xc19bf174cf692694ULL,
0xe49b69c19ef14ad2ULL,0xefbe4786384f25e3fULL,0x0fc19dc68b8cd5b5ULL,0x240ca1cc77ac9c65ULL,
0x2de92c6f592b0275ULL,0x4a7484aa6ea6e483ULL,0x5cb0a9dcbd41fbd4ULL,0x76f988da831153b5ULL,
0x983e5152ee66dfabULL,0xa831c66d2db43210ULL,0xb00327c898fb213fULL,0xbf597fc7beef0ee4ULL,
0xc6e00bf33da88fc2ULL,0xd5a79147930aa725ULL,0x06ca6351e003826fULL,0x142929670a0e6e70ULL,
0x27b70a8546d22ffcULL,0x2e1b21385c26c926ULL,0x4d2c6dfc5ac42aedULL,0x53380d139d95b3dfULL,
0x650a73548baf63deULL,0x766a0abb3c77b2a8ULL,0x81c2c92e47edaee6ULL,0x92722c851482353bULL,
0xa2bfe8a14cf10364ULL,0xa81a664bbc423001ULL,0xc24b8b70d0f89791ULL,0xc76c51a30654be30ULL,
0xd192e819d6ef5218ULL,0xd69906245565a910ULL,0xf40e35855771202aULL,0x106aa07032bbd1b8ULL,
0x19a4c116b8d2d0c8ULL,0x1e376c085141ab53ULL,0x2748774cdf8eeb99ULL,0x34b0bcb5e19b48a8ULL,
0x391c0cb3c5c95a63ULL,0x4ed8aa4ae3418acbULL,0x5b9cca4f7763e373ULL,0x682e6ff3d6b2b8b3ULL,
0x748f82ee5defb2fcULL,0x78a5636f43172f60ULL,0x84c87814a1f0ab72ULL,0x8cc702081a6439ecULL,
0x90befffa23631e28ULL,0xa4506cebde82bde9ULL,0xbef9a3f7b2c67915ULL,0xc67178f2e372532bULL,
0xca273eceea26619cULL,0xd186b8c721c0c207ULL,0xeada7dd6cde0eb1eULL,0xf57d4f7fee6ed178ULL,
0x06f067aa72176fbaULL,0x0a637dc5a2c898a6ULL,0x113f9804bef90daeULL,0x1b710b35131c471bULL,
0x28db77f523047d84ULL,0x32caab7b40c72493ULL,0x3c9ebe0a15c9bebcULL,0x431d67c49c100d4cULL,
0x4cc5d4becb3e42b6ULL,0x597f299cfc657e2aULL,0x5fcb6fab3ad6faecULL,0x6c44198c4a475817ULL};

__device__ void sha512_compress(uint64_t s[8], const uint8_t block[128]){
    uint64_t w[80];
    for(int i=0;i<16;i++){uint64_t v=0; for(int j=0;j<8;j++) v=(v<<8)|block[i*8+j]; w[i]=v;}
    #pragma unroll
    for(int i=16;i<80;i++) w[i]=SS1(w[i-2])+w[i-7]+SS0(w[i-15])+w[i-16];
    uint64_t a=s[0],b=s[1],c=s[2],d=s[3],e=s[4],f=s[5],g=s[6],h=s[7];
    #pragma unroll
    for(int i=0;i<80;i++){uint64_t t1=h+BS1(e)+Ch(e,f,g)+K[i]+w[i]; uint64_t t2=BS0(a)+Maj(a,b,c); h=g;g=f;f=e;e=d+t1;d=c;c=b;b=a;a=t1+t2;}
    s[0]+=a;s[1]+=b;s[2]+=c;s[3]+=d;s[4]+=e;s[5]+=f;s[6]+=g;s[7]+=h;
}

__device__ void sha512_init(uint64_t s[8]){
 s[0]=0x6a09e667f3bcc908ULL;s[1]=0xbb67ae8584caa73bULL;s[2]=0x3c6ef372fe94f82bULL;s[3]=0xa54ff53a5f1d36f1ULL;
 s[4]=0x510e527fade682d1ULL;s[5]=0x9b05688c2b3e6c1fULL;s[6]=0x1f83d9abfb41bd6bULL;s[7]=0x5be0cd19137e2179ULL;
}

__device__ void sha512_oneblock(uint64_t out[8], const uint8_t *msg, int len){
 uint8_t b[128]={0}; for(int i=0;i<len;i++) b[i]=msg[i]; b[len]=0x80;
 uint64_t bits=(uint64_t)len*8; for(int i=0;i<8;i++) b[120+i]=(uint8_t)(bits>>(56-8*i));
 sha512_init(out); sha512_compress(out,b);
}

__device__ void hmac_sha512_key(const uint8_t *key,int keylen,const uint8_t *msg,int msglen,uint8_t out[64]){
 uint8_t k[128]={0};
 if(keylen>128){uint64_t s[8];sha512_oneblock(s,key,keylen);for(int i=0;i<8;i++)for(int j=0;j<8;j++)k[i*8+j]=(uint8_t)(s[i]>>(56-8*j));}
 else for(int i=0;i<keylen;i++)k[i]=key[i];
 uint8_t ib[128+256]; uint8_t ob[128+64];
 for(int i=0;i<128;i++){ib[i]=k[i]^0x36;ob[i]=k[i]^0x5c;} for(int i=0;i<msglen;i++)ib[128+i]=msg[i];
 uint64_t si[8];
 // Inner can span two blocks for the fixed PBKDF2 message sizes used here.
 uint8_t inner[64];
 uint8_t tmp[384]; for(int i=0;i<128+msglen;i++)tmp[i]=ib[i];
 int ilen=128+msglen; uint64_t st[8]; sha512_init(st);
 int p=0; while(ilen-p>=128){sha512_compress(st,tmp+p);p+=128;}
 uint8_t last[128]={0};int rem=ilen-p;for(int i=0;i<rem;i++)last[i]=tmp[p+i];last[rem]=0x80;uint64_t bl=(uint64_t)ilen*8;for(int i=0;i<8;i++)last[120+i]=(uint8_t)(bl>>(56-8*i));sha512_compress(st,last);
 for(int i=0;i<8;i++)for(int j=0;j<8;j++)inner[i*8+j]=(uint8_t)(st[i]>>(56-8*j));
 uint8_t outer[192];
 for(int i=0;i<128;i++)outer[i]=k[i]^0x5c;
 for(int i=0;i<64;i++)outer[128+i]=inner[i];
 uint64_t sout[8]; sha512_init(sout); sha512_compress(sout,outer);
 uint8_t fin[128]={0};
 for(int i=0;i<64;i++) fin[i]=inner[i];
 fin[64]=0x80;
 uint64_t total=192ULL*8ULL;
 for(int i=0;i<8;i++) fin[120+i]=(uint8_t)(total>>(56-8*i));
 sha512_compress(sout,fin);
 for(int i=0;i<8;i++)for(int j=0;j<8;j++)out[i*8+j]=(uint8_t)(sout[i]>>(56-8*j));
 (void)ob;
}

// PBKDF2-HMAC-SHA512, c=2048, dkLen=64. One thread handles one mnemonic.
__global__ void pbkdf2_kernel(const uint32_t *passwords,const uint32_t *lengths,const uint8_t *salt,uint32_t salt_len,uint8_t *out,uint32_t count){
 uint32_t id=blockIdx.x*blockDim.x+threadIdx.x;if(id>=count)return;
 uint8_t pass[128];int plen=(int)lengths[id];for(int i=0;i<plen;i++)pass[i]=(uint8_t)(passwords[id*32+i/4]>>(8*(i%4)));
 uint8_t msg[128]; for(int i=0;i<salt_len;i++)msg[i]=salt[i]; msg[salt_len]=0;msg[salt_len+1]=0;msg[salt_len+2]=0;msg[salt_len+3]=1;
 uint8_t u[64],t[64]; hmac_sha512_key(pass,plen,msg,salt_len+4,u);for(int i=0;i<64;i++)t[i]=u[i];
 for(int iter=1;iter<2048;iter++){hmac_sha512_key(pass,plen,u,64,u);for(int i=0;i<64;i++)t[i]^=u[i];}
 for(int i=0;i<64;i++)out[id*64+i]=t[i];
}

extern "C" int ton_cuda_device_count(){int n=0;cudaError_t e=cudaGetDeviceCount(&n);return e==cudaSuccess?n:-1;}

// Report a per-device candidate capacity from currently free device memory.
// The PBKDF2 call holds 128-byte passwords, 4-byte lengths, and 64-byte
// outputs simultaneously; reserve 25% of free VRAM for other device work.
extern "C" int ton_cuda_max_batch_size(uint64_t *out){
 if(!out)return (int)cudaErrorInvalidValue;
 size_t free_bytes=0,total_bytes=0;
 CUDA_OK(cudaMemGetInfo(&free_bytes,&total_bytes));
 (void)total_bytes;
 uint64_t usable=(uint64_t)free_bytes-(uint64_t)free_bytes/4ULL;
 uint64_t count=usable/196ULL;
 // Keep (count + 127) safe in the CUDA launch expression and fit the u32
 // candidate argument used by the kernel.
 if(count>0x7fffff80ULL)count=0x7fffff80ULL;
 if(count==0)return (int)cudaErrorMemoryAllocation;
 *out=count;
 return (int)cudaSuccess;
}

extern "C" int ton_cuda_pbkdf2(const uint32_t *h_passwords,const uint32_t *h_lengths,uint32_t count,const uint8_t *h_salt,uint32_t salt_len,uint8_t *h_out){
 if(!h_passwords||!h_lengths||!h_salt||!h_out||count==0||count>0x7fffff80U||salt_len==0||salt_len>107)return (int)cudaErrorInvalidValue;
 uint32_t *d_pass=nullptr,*d_len=nullptr;uint8_t *d_salt=nullptr,*d_out=nullptr;cudaError_t e;
 e=cudaMalloc(&d_pass,(size_t)count*32*4);if(e!=cudaSuccess)goto cleanup;
 e=cudaMalloc(&d_len,(size_t)count*4);if(e!=cudaSuccess)goto cleanup;
 e=cudaMalloc(&d_salt,salt_len);if(e!=cudaSuccess)goto cleanup;
 e=cudaMalloc(&d_out,(size_t)count*64);if(e!=cudaSuccess)goto cleanup;
 e=cudaMemcpy(d_pass,h_passwords,(size_t)count*32*4,cudaMemcpyHostToDevice);if(e!=cudaSuccess)goto cleanup;
 e=cudaMemcpy(d_len,h_lengths,(size_t)count*4,cudaMemcpyHostToDevice);if(e!=cudaSuccess)goto cleanup;
 e=cudaMemcpy(d_salt,h_salt,salt_len,cudaMemcpyHostToDevice);if(e!=cudaSuccess)goto cleanup;
 pbkdf2_kernel<<<(count+127)/128,128>>>(d_pass,d_len,d_salt,salt_len,d_out,count);
 e=cudaGetLastError();if(e!=cudaSuccess)goto cleanup;
 e=cudaDeviceSynchronize();if(e!=cudaSuccess)goto cleanup;
 e=cudaMemcpy(h_out,d_out,(size_t)count*64,cudaMemcpyDeviceToHost);

 cleanup:
 // Clear secret-bearing allocations before freeing them. Preserve the original
 // CUDA failure, but report a wipe failure when computation otherwise succeeded.
 {
  cudaError_t wipe=cudaSuccess;
  if(d_pass){wipe=cudaMemset(d_pass,0,(size_t)count*32*4);if(e==cudaSuccess&&wipe!=cudaSuccess)e=wipe;}
  if(d_len){wipe=cudaMemset(d_len,0,(size_t)count*4);if(e==cudaSuccess&&wipe!=cudaSuccess)e=wipe;}
  if(d_salt){wipe=cudaMemset(d_salt,0,salt_len);if(e==cudaSuccess&&wipe!=cudaSuccess)e=wipe;}
  if(d_out){wipe=cudaMemset(d_out,0,(size_t)count*64);if(e==cudaSuccess&&wipe!=cudaSuccess)e=wipe;}
  wipe=cudaDeviceSynchronize();if(e==cudaSuccess&&wipe!=cudaSuccess)e=wipe;
 }
 if(d_pass){cudaError_t f=cudaFree(d_pass);if(e==cudaSuccess&&f!=cudaSuccess)e=f;}
 if(d_len){cudaError_t f=cudaFree(d_len);if(e==cudaSuccess&&f!=cudaSuccess)e=f;}
 if(d_salt){cudaError_t f=cudaFree(d_salt);if(e==cudaSuccess&&f!=cudaSuccess)e=f;}
 if(d_out){cudaError_t f=cudaFree(d_out);if(e==cudaSuccess&&f!=cudaSuccess)e=f;}
 return (int)e;
}
