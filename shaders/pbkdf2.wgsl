// BIP39 PBKDF2-HMAC-SHA512 compute kernel.
// Input passwords are UTF-8 bytes packed little-endian into 32 u32 words per candidate.
// The salt is "mnemonic" + passphrase and is shared by all candidates.
// This stage intentionally stops after BIP39 seed generation; the CPU reference then
// performs SLIP-0010, Ed25519 and TON StateInit hashing until those kernels are proven
// equivalent on every supported GPU family.

struct Params {
    count: u32,
    salt_len: u32,
    _pad0: u32,
    _pad1: u32,
    salt: array<vec4<u32>, 16>, // 64 packed u32 words; vec4 gives 16-byte uniform-array stride
}

struct Output {
    seeds: array<u32>, // 16 u32 per candidate, big-endian byte order within each word
}
const ZERO_BLOCK: array<vec2<u32>,16> = array<vec2<u32>,16>(vec2<u32>(0u,0u),vec2<u32>(0u,0u),vec2<u32>(0u,0u),vec2<u32>(0u,0u),vec2<u32>(0u,0u),vec2<u32>(0u,0u),vec2<u32>(0u,0u),vec2<u32>(0u,0u),vec2<u32>(0u,0u),vec2<u32>(0u,0u),vec2<u32>(0u,0u),vec2<u32>(0u,0u),vec2<u32>(0u,0u),vec2<u32>(0u,0u),vec2<u32>(0u,0u),vec2<u32>(0u,0u));


@group(0) @binding(0) var<storage, read> passwords: array<u32>;
@group(0) @binding(1) var<storage, read> lengths: array<u32>;
@group(0) @binding(2) var<uniform> params: Params;
@group(0) @binding(3) var<storage, read_write> output_data: Output;

const K: array<vec2<u32>, 80> = array<vec2<u32>, 80>(
    vec2<u32>(0x428a2f98u,0xd728ae22u),vec2<u32>(0x71374491u,0x23ef65cdu),
    vec2<u32>(0xb5c0fbcfu,0xec4d3b2fu),vec2<u32>(0xe9b5dba5u,0x8189dbbcu),
    vec2<u32>(0x3956c25bu,0xf348b538u),vec2<u32>(0x59f111f1u,0xb605d019u),
    vec2<u32>(0x923f82a4u,0xaf194f9bu),vec2<u32>(0xab1c5ed5u,0xda6d8118u),
    vec2<u32>(0xd807aa98u,0xa3030242u),vec2<u32>(0x12835b01u,0x45706fbeu),
    vec2<u32>(0x243185beu,0x4ee4b28cu),vec2<u32>(0x550c7dc3u,0xd5ffb4e2u),
    vec2<u32>(0x72be5d74u,0xf27b896fu),vec2<u32>(0x80deb1feu,0x3b1696b1u),
    vec2<u32>(0x9bdc06a7u,0x25c71235u),vec2<u32>(0xc19bf174u,0xcf692694u),
    vec2<u32>(0xe49b69c1u,0x9ef14ad2u),vec2<u32>(0xefbe4786u,0x384f25e3u),
    vec2<u32>(0x0fc19dc6u,0x8b8cd5b5u),vec2<u32>(0x240ca1ccu,0x77ac9c65u),
    vec2<u32>(0x2de92c6fu,0x592b0275u),vec2<u32>(0x4a7484aau,0x6ea6e483u),
    vec2<u32>(0x5cb0a9dcu,0xbd41fbd4u),vec2<u32>(0x76f988dau,0x831153b5u),
    vec2<u32>(0x983e5152u,0xee66dfabu),vec2<u32>(0xa831c66du,0x2db43210u),
    vec2<u32>(0xb00327c8u,0x98fb213fu),vec2<u32>(0xbf597fc7u,0xbeef0ee4u),
    vec2<u32>(0xc6e00bf3u,0x3da88fc2u),vec2<u32>(0xd5a79147u,0x930aa725u),
    vec2<u32>(0x06ca6351u,0xe003826fu),vec2<u32>(0x14292967u,0x0a0e6e70u),
    vec2<u32>(0x27b70a85u,0x46d22ffcu),vec2<u32>(0x2e1b2138u,0x5c26c926u),
    vec2<u32>(0x4d2c6dfcu,0x5ac42aedu),vec2<u32>(0x53380d13u,0x9d95b3dfu),
    vec2<u32>(0x650a7354u,0x8baf63deu),vec2<u32>(0x766a0abbu,0x3c77b2a8u),
    vec2<u32>(0x81c2c92eu,0x47edaee6u),vec2<u32>(0x92722c85u,0x1482353bu),
    vec2<u32>(0xa2bfe8a1u,0x4cf10364u),vec2<u32>(0xa81a664bu,0xbc423001u),
    vec2<u32>(0xc24b8b70u,0xd0f89791u),vec2<u32>(0xc76c51a3u,0x0654be30u),
    vec2<u32>(0xd192e819u,0xd6ef5218u),vec2<u32>(0xd6990624u,0x5565a910u),
    vec2<u32>(0xf40e3585u,0x5771202au),vec2<u32>(0x106aa070u,0x32bbd1b8u),
    vec2<u32>(0x19a4c116u,0xb8d2d0c8u),vec2<u32>(0x1e376c08u,0x5141ab53u),
    vec2<u32>(0x2748774cu,0xdf8eeb99u),vec2<u32>(0x34b0bcb5u,0xe19b48a8u),
    vec2<u32>(0x391c0cb3u,0xc5c95a63u),vec2<u32>(0x4ed8aa4au,0xe3418acbu),
    vec2<u32>(0x5b9cca4fu,0x7763e373u),vec2<u32>(0x682e6ff3u,0xd6b2b8a3u),
    vec2<u32>(0x748f82eeu,0x5defb2fcu),vec2<u32>(0x78a5636fu,0x43172f60u),
    vec2<u32>(0x84c87814u,0xa1f0ab72u),vec2<u32>(0x8cc70208u,0x1a6439ecu),
    vec2<u32>(0x90befffau,0x23631e28u),vec2<u32>(0xa4506cebu,0xde82bde9u),
    vec2<u32>(0xbef9a3f7u,0xb2c67915u),vec2<u32>(0xc67178f2u,0xe372532bu),
    vec2<u32>(0xca273eceu,0xea26619cu),vec2<u32>(0xd186b8c7u,0x21c0c207u),
    vec2<u32>(0xeada7dd6u,0xcde0eb1eu),vec2<u32>(0xf57d4f7fu,0xee6ed178u),
    vec2<u32>(0x06f067aau,0x72176fbau),vec2<u32>(0x0a637dc5u,0xa2c898a6u),
    vec2<u32>(0x113f9804u,0xbef90daeu),vec2<u32>(0x1b710b35u,0x131c471bu),
    vec2<u32>(0x28db77f5u,0x23047d84u),vec2<u32>(0x32caab7bu,0x40c72493u),
    vec2<u32>(0x3c9ebe0au,0x15c9bebcu),vec2<u32>(0x431d67c4u,0x9c100d4cu),
    vec2<u32>(0x4cc5d4beu,0xcb3e42b6u),vec2<u32>(0x597f299cu,0xfc657e2au),
    vec2<u32>(0x5fcb6fabu,0x3ad6faecu),vec2<u32>(0x6c44198cu,0x4a475817u)
);

const IV: array<vec2<u32>, 8> = array<vec2<u32>, 8>(
    vec2<u32>(0x6a09e667u,0xf3bcc908u),vec2<u32>(0xbb67ae85u,0x84caa73bu),
    vec2<u32>(0x3c6ef372u,0xfe94f82bu),vec2<u32>(0xa54ff53au,0x5f1d36f1u),
    vec2<u32>(0x510e527fu,0xade682d1u),vec2<u32>(0x9b05688cu,0x2b3e6c1fu),
    vec2<u32>(0x1f83d9abu,0xfb41bd6bu),vec2<u32>(0x5be0cd19u,0x137e2179u)
);

fn add64(a: vec2<u32>, b: vec2<u32>) -> vec2<u32> {
    let lo = a.y + b.y;
    let carry = select(0u, 1u, lo < a.y);
    return vec2<u32>(a.x + b.x + carry, lo);
}
fn add5(a: vec2<u32>,b:vec2<u32>,c:vec2<u32>,d:vec2<u32>,e:vec2<u32>) -> vec2<u32> {
    return add64(add64(add64(add64(a,b),c),d),e);
}
fn rotr(x:vec2<u32>, n:u32)->vec2<u32>{
    let s=n%64u;
    if(s==0u){return x;}
    if(s==32u){return vec2<u32>(x.y,x.x);}
    if(s<32u){return vec2<u32>((x.x>>s)|(x.y<<(32u-s)),(x.y>>s)|(x.x<<(32u-s)));}
    let r=s-32u;
    return vec2<u32>((x.y>>r)|(x.x<<(32u-r)),(x.x>>r)|(x.y<<(32u-r)));
}
fn shr(x:vec2<u32>, n:u32)->vec2<u32>{
    if(n==0u){return x;}
    if(n<32u){return vec2<u32>(x.x>>n,(x.y>>n)|(x.x<<(32u-n)));}
    return vec2<u32>(0u,x.x>>(n-32u));
}
fn ch(x:vec2<u32>,y:vec2<u32>,z:vec2<u32>)->vec2<u32>{return (x&y)^((~x)&z);}
fn maj(x:vec2<u32>,y:vec2<u32>,z:vec2<u32>)->vec2<u32>{return (x&y)^(x&z)^(y&z);}
fn bs0(x:vec2<u32>)->vec2<u32>{return rotr(x,28u)^rotr(x,34u)^rotr(x,39u);}
fn bs1(x:vec2<u32>)->vec2<u32>{return rotr(x,14u)^rotr(x,18u)^rotr(x,41u);}
fn ss0(x:vec2<u32>)->vec2<u32>{return rotr(x,1u)^rotr(x,8u)^shr(x,7u);}
fn ss1(x:vec2<u32>)->vec2<u32>{return rotr(x,19u)^rotr(x,61u)^shr(x,6u);}

fn load_be(words: ptr<function,array<vec2<u32>,16>>, i:u32, b0:u32,b1:u32,b2:u32,b3:u32,b4:u32,b5:u32,b6:u32,b7:u32){
    (*words)[i]=vec2<u32>((b0<<24u)|(b1<<16u)|(b2<<8u)|b3,(b4<<24u)|(b5<<16u)|(b6<<8u)|b7);
}

fn compress(block: array<vec2<u32>,16>, initial: array<vec2<u32>,8>) -> array<vec2<u32>,8> {
    // SHA-512 only needs the previous 16 schedule words. Keeping an 80-word
    // array per invocation creates high register pressure and spills on older GPUs.
    var w:array<vec2<u32>,16>;
    for(var i=0u;i<16u;i++){w[i]=block[i];}
    var a=initial[0]; var b=initial[1]; var c=initial[2]; var d=initial[3];
    var e=initial[4]; var f=initial[5]; var g=initial[6]; var h=initial[7];
    for(var i=0u;i<80u;i++){
        let idx=i&15u;
        if(i>=16u){
            let w_im16=w[idx];
            w[idx]=add64(add64(add64(ss1(w[(i-2u)&15u]),w[(i-7u)&15u]),ss0(w[(i-15u)&15u])),w_im16);
        }
        let wi=w[idx];
        let t1=add5(h,bs1(e),ch(e,f,g),K[i],wi);
        let t2=add64(bs0(a),maj(a,b,c));
        h=g; g=f; f=e; e=add64(d,t1); d=c; c=b; b=a; a=add64(t1,t2);
    }
    var out:array<vec2<u32>,8>;
    out[0]=add64(initial[0],a);out[1]=add64(initial[1],b);out[2]=add64(initial[2],c);out[3]=add64(initial[3],d);
    out[4]=add64(initial[4],e);out[5]=add64(initial[5],f);out[6]=add64(initial[6],g);out[7]=add64(initial[7],h);
    return out;
}

fn byte_at(base:u32, index:u32)->u32{
    let word=passwords[base + (index>>2u)];
    return (word >> ((index&3u)*8u)) & 255u;
}
fn salt_byte(index:u32)->u32{
    let packed = params.salt[index >> 4u];
    let word = packed[(index >> 2u) & 3u];
    return (word >> ((index&3u)*8u)) & 255u;
}

fn hmac_key_states(base:u32, len:u32) -> array<array<vec2<u32>,8>,2> {
    var inner_block:array<vec2<u32>,16>;
    var outer_block:array<vec2<u32>,16>;
    for(var i=0u;i<16u;i++){inner_block[i]=vec2<u32>(0u,0u);outer_block[i]=vec2<u32>(0u,0u);}
    for(var i=0u;i<128u;i++){
        let kb=select(0u,byte_at(base,i),i<len);
        let w=i>>3u; let sh=(7u-(i&7u))*8u;
        if(sh>=32u){
            inner_block[w].x |= kb << (sh-32u);
            outer_block[w].x |= kb << (sh-32u);
        } else {
            inner_block[w].y |= kb << sh;
            outer_block[w].y |= kb << sh;
        }
    }
    for(var i=0u;i<16u;i++){
        inner_block[i].x ^= 0x36363636u; inner_block[i].y ^= 0x36363636u;
        outer_block[i].x ^= 0x5c5c5c5cu; outer_block[i].y ^= 0x5c5c5c5cu;
    }
    var r:array<array<vec2<u32>,8>,2>;
    r[0]=compress(inner_block,IV); r[1]=compress(outer_block,IV); return r;
}

fn digest_byte(d:array<vec2<u32>,8>, index:u32)->u32 {
    let p=d[index>>3u];
    let sh=(7u-(index&7u))*8u;
    if(sh>=32u){return (p.x>>(sh-32u))&255u;}
    return (p.y>>sh)&255u;
}

fn put_byte(block:ptr<function,array<vec2<u32>,16>>, index:u32, v:u32) {
    let w=index>>3u; let sh=(7u-(index&7u))*8u;
    if(sh>=32u){(*block)[w].x |= v << (sh-32u);} else {(*block)[w].y |= v << sh;}
}

fn second_block(message_len:u32, msg_kind:u32, initial_state:array<vec2<u32>,8>, previous_u:array<vec2<u32>,8>) -> array<vec2<u32>,8> {
    var block = ZERO_BLOCK;
    if(msg_kind==0u){
        // U1 is salt || INT32_BE(1); this is the only PBKDF2 round that
        // needs byte-wise salt packing.
        for(var i=0u;i<params.salt_len;i++){
            put_byte(&block,i,salt_byte(i));
        }
        put_byte(&block,params.salt_len+3u,1u);
    } else {
        // Later U values are already eight big-endian SHA-512 words.
        for(var i=0u;i<8u;i++){
            block[i]=previous_u[i];
        }
    }
    put_byte(&block,message_len,0x80u);
    // The HMAC ipad/opad block has already been compressed.
    let bit_lo=(128u+message_len)*8u;
    block[15]=vec2<u32>(0u,bit_lo);
    return compress(block,initial_state);
}

fn hmac_step(inner_state:array<vec2<u32>,8>, outer_state:array<vec2<u32>,8>, msg_kind:u32, previous:array<vec2<u32>,8>) -> array<vec2<u32>,8> {
    let msg_len=select(params.salt_len+4u,64u,msg_kind==1u);
    let inner=second_block(msg_len,msg_kind,inner_state,previous);
    var block = ZERO_BLOCK;
    for(var i=0u;i<8u;i++){block[i]=inner[i];}
    block[8]=vec2<u32>(0x80000000u,0u);
    // SHA-512 HMAC outer hash processes 128-byte opad + 64-byte inner digest = 192 bytes.
    block[15]=vec2<u32>(0u,192u*8u);
    return compress(block,outer_state);
}

@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) gid:vec3<u32>) {
    let i=gid.x;
    if(i>=params.count){return;}
    let base=i*32u;
    let states=hmac_key_states(base,lengths[i]);
    var zero:array<vec2<u32>,8>;
    for(var z=0u;z<8u;z++){zero[z]=vec2<u32>(0u,0u);}
    var u=hmac_step(states[0],states[1],0u,zero);
    var t=u;
    for(var iter=1u;iter<2048u;iter++){
        u=hmac_step(states[0],states[1],1u,u);
        for(var j=0u;j<8u;j++){t[j]=t[j]^u[j];}
    }
    let out=i*16u;
    for(var j=0u;j<8u;j++){output_data.seeds[out+j*2u]=t[j].x;output_data.seeds[out+j*2u+1u]=t[j].y;}
}
