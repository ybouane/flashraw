import struct, numpy as np
def write_dng(path, mosaic_u16, pattern, black=512):
    """Minimal uncompressed CFA DNG; camera space = linear sRGB (ColorMatrix = XYZ->sRGB), neutral WB."""
    h, w = mosaic_u16.shape
    P = pattern.shape[0]
    data = mosaic_u16.astype('<u2').tobytes()
    xyz2srgb = [3.2404542,-1.5371385,-0.4985314,-0.9692660,1.8760108,0.0415560,0.0556434,-0.2040259,1.0572252]
    ents = []  # (tag,type,count,value bytes)
    def S(t,v): ents.append((t,3,len(v) if hasattr(v,'__len__') else 1,struct.pack('<%dH'%(len(v) if hasattr(v,'__len__') else 1),*(v if hasattr(v,'__len__') else [v]))))
    def L(t,v): ents.append((t,4,1,struct.pack('<I',v)))
    def B(t,v): ents.append((t,1,len(v),bytes(v)))
    def A(t,s): ents.append((t,2,len(s)+1,s.encode()+b'\0'))
    def R(t,v): ents.append((t,5,len(v),b''.join(struct.pack('<II',int(round(x*1000000)),1000000) for x in v)))
    def SR(t,v): ents.append((t,10,len(v),b''.join(struct.pack('<ii',int(round(x*1000000)),1000000) for x in v)))
    L(254,0); L(256,w); L(257,h); S(258,16); S(259,1); S(262,32803); L(273,0); S(277,1); S(278,h) if False else L(278,h); L(279,len(data))
    S(284,1); S(33421,[P,P]); B(33422,[int(x) for x in pattern.flatten()])
    A(50708,'Bench Synthetic'); B(50706,[1,4,0,0]); B(50707,[1,1,0,0])
    SR(50721,xyz2srgb); R(50728,[1,1,1]); S(50778,21); L(50714,black); L(50717,65535)
    ents.sort(key=lambda e:e[0])
    n=len(ents); ifd_off=8; ifd_size=2+12*n+4; extra_off=ifd_off+ifd_size
    extra=b''; table=b''
    # data placed after extras; compute sizes first
    ext_sizes=sum((len(e[3])+1)&~1 for e in ents if len(e[3])>4)
    data_off=extra_off+ext_sizes
    for t,ty,c,v in ents:
        if t==273: v=struct.pack('<I',data_off)
        if len(v)>4:
            table+=struct.pack('<HHII',t,ty,c,extra_off+len(extra)); extra+=v+(b'\0' if len(v)&1 else b'')
        else:
            table+=struct.pack('<HHI',t,ty,c)+v.ljust(4,b'\0')
    with open(path,'wb') as f:
        f.write(b'II*\0'+struct.pack('<I',8)+struct.pack('<H',n)+table+struct.pack('<I',0)+extra+data)
