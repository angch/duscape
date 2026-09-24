; PNG.ASM, included by DISKONAU.ASM: a PNG decoded into the preview's half blocks (PREVIEW.ASM).
;
; Every colour type and bit depth, Adam7 too, palette transparency (tRNS). Inflate follows Mark
; Adler's puff: canonical Huffman codes decoded a bit at a time, a 32 KiB window in winseg. Each
; line is unfiltered in lineseg (two line buffers, swapped) and its pixels averaged into acc,
; every step_x-th of every step_y-th row: a block pixel still gets dozens of them.
;
; Registers while decoding: ES = lineseg, FS = winseg, DS = data.

PNG_MAXLINE     = 7FF0h         ; bytes in a line, filter byte included

png_decode:
        mov     [acc_mode],0
        mov     cx,8                    ; the signature, known already
@@:     call    rd_byte
        loop    @b
        mov     di,pal_a
        mov     al,255
        mov     cx,256
        rep     stosb
        mov     [png_hdr],0
.chunk: call    rd_be32
        mov     [chunk_len],eax
        call    rd_be32                 ; the type, read big-endian: 'IHDR' is 'RDHI' to FASM
        cmp     eax,'RDHI'
        je      .ihdr
        cmp     eax,'ETLP'
        je      .plte
        cmp     eax,'SNRt'
        je      .trns
        cmp     eax,'TADI'
        je      .idat
        cmp     eax,'DNEI'
        je      .noidat
.skip:  mov     ecx,[chunk_len]
        add     ecx,4                   ; and the CRC
        call    rd_skip
        jmp     .chunk
.noidat:
        mov     ax,s_png_noidat
        jmp     dec_fail
.ihdr:  call    rd_be32
        mov     [img_w],eax
        call    rd_be32
        mov     [img_h],eax
        call    rd_byte
        mov     [png_depth],al
        call    rd_byte
        mov     [png_ctype],al
        call    rd_byte                 ; compression
        mov     ah,al
        call    rd_byte                 ; filter method
        or      ah,al
        jnz     .bad
        call    rd_byte
        mov     [png_interlace],al
        cmp     al,1
        ja      .bad
        mov     ecx,[chunk_len]
        sub     ecx,13
        jb      .bad
        add     ecx,4
        call    rd_skip
        call    png_format
        mov     [png_hdr],1
        jmp     .chunk
.plte:  mov     ecx,[chunk_len]
        xor     bx,bx
@@:     jecxz   .crc
        call    rd_byte
        cmp     bx,768
        jae     .pnext
        mov     [pal+bx],al
        inc     bx
.pnext: dec     ecx
        jmp     @b
.crc:   mov     ecx,4
        call    rd_skip
        jmp     .chunk
.trns:  cmp     [png_ctype],3
        jne     .skip
        mov     ecx,[chunk_len]
        xor     bx,bx
@@:     jecxz   .crc
        call    rd_byte
        cmp     bx,256
        jae     .tn
        mov     [pal_a+bx],al
        inc     bx
.tn:    dec     ecx
        jmp     @b
.idat:  cmp     [png_hdr],0
        je      .bad
        mov     eax,[chunk_len]
        mov     [idat_left],eax
        call    acc_setup
        mov     es,[lineseg]
        mov     fs,[winseg]
        call    png_first_pass
        call    inflate
        cmp     [png_done],0
        je      .trunc
        clc
        ret
.trunc: mov     ax,s_truncated
        jmp     dec_fail
.bad:   mov     ax,s_png_bad
        jmp     dec_fail

; the header checked, the description written, the pixel size worked out
png_format:
        mov     eax,[img_w]
        dec     eax
        cmp     eax,40000
        jae     .big
        mov     eax,[img_h]
        dec     eax
        cmp     eax,40000
        jae     .big
        mov     al,[png_ctype]
        mov     cl,1                    ; channels
        cmp     al,0
        je      .ch
        mov     cl,3
        cmp     al,2
        je      .ch
        mov     cl,1
        cmp     al,3
        je      .ch
        mov     cl,2
        cmp     al,4
        je      .ch
        mov     cl,4
        cmp     al,6
        jne     .bad
.ch:    mov     [png_chans],cl
        mov     al,[png_depth]
        cmp     al,1
        je      .d
        cmp     al,2
        je      .d
        cmp     al,4
        je      .d
        cmp     al,8
        je      .d
        cmp     al,16
        jne     .bad
.d:     mul     cl
        mov     [png_bpx],ax            ; bits a pixel
        add     ax,7
        shr     ax,3
        mov     [png_bpp],ax            ; bytes a pixel, at least one (for the filters)
        ; "PNG 1920x1080"
        push    es
        push    ds
        pop     es
        mov     di,pv_desc
        mov     si,s_png
        call    append_z
        mov     eax,[img_w]
        call    u32dec
        mov     al,'x'
        stosb
        mov     eax,[img_h]
        call    u32dec
        mov     byte [di],0
        pop     es
        ret
.big:   mov     ax,s_too_large
        jmp     dec_fail
.bad:   mov     ax,s_png_bad
        jmp     dec_fail

; Adam7: x0, y0, dx, dy of each pass
adam7   db 0,0,8,8, 4,0,8,8, 0,4,4,8, 2,0,4,4, 0,2,2,4, 1,0,2,2, 0,1,1,2

png_first_pass:
        mov     [png_done],0
        mov     [png_pass],-1
        cmp     [png_interlace],0
        jne     png_next_pass
        mov     dword [pass_x0],0
        mov     dword [pass_y0],0
        mov     dword [pass_dx],1
        mov     dword [pass_dy],1
        mov     eax,[img_w]
        mov     [pass_w],eax
        mov     eax,[img_h]
        mov     [pass_h],eax
        mov     [png_pass],7
        jmp     png_start_pass

; the next pass that has pixels, or done
png_next_pass:
        inc     [png_pass]
        cmp     [png_pass],7
        jae     .done
        movzx   bx,[png_pass]
        shl     bx,2
        movzx   eax,byte [cs:adam7+bx]
        mov     [pass_x0],eax
        movzx   eax,byte [cs:adam7+bx+1]
        mov     [pass_y0],eax
        movzx   eax,byte [cs:adam7+bx+2]
        mov     [pass_dx],eax
        movzx   eax,byte [cs:adam7+bx+3]
        mov     [pass_dy],eax
        ; ceil((W - x0) / dx), ceil((H - y0) / dy)
        mov     eax,[img_w]
        sub     eax,[pass_x0]
        jbe     png_next_pass
        add     eax,[pass_dx]
        dec     eax
        xor     edx,edx
        div     dword [pass_dx]
        mov     [pass_w],eax
        mov     eax,[img_h]
        sub     eax,[pass_y0]
        jbe     png_next_pass
        add     eax,[pass_dy]
        dec     eax
        xor     edx,edx
        div     dword [pass_dy]
        mov     [pass_h],eax
        jmp     png_start_pass
.done:  mov     [png_done],1
        ret

png_start_pass:
        mov     eax,[pass_w]
        movzx   ecx,[png_bpx]
        mul     ecx
        add     eax,7
        shr     eax,3
        cmp     eax,PNG_MAXLINE
        jae     .wide
        mov     [line_bytes],ax
        inc     ax
        mov     [line_len],ax
        mov     dword [pass_row],0
        mov     [lpos],0
        mov     [cur_base],8000h
        mov     [prev_base],0
        xor     di,di                   ; the line above the first is zeros
        mov     cx,[line_len]
        xor     al,al
        rep     stosb
        ; sampling in the pass's own pixels
        mov     eax,[step_x]
        xor     edx,edx
        div     dword [pass_dx]
        test    eax,eax
        jnz     @f
        inc     eax
@@:     mov     [pstep_x],eax
        mov     eax,[step_y]
        xor     edx,edx
        div     dword [pass_dy]
        test    eax,eax
        jnz     @f
        inc     eax
@@:     mov     [pstep_y],eax
        ret
.wide:  mov     ax,s_too_wide
        jmp     dec_fail

; al = the next inflated byte: into the line, and the line out when it is whole
png_put:
        cmp     [png_done],0
        jne     .r
        push    bx
        mov     bx,[lpos]
        add     bx,[cur_base]
        mov     [es:bx],al
        inc     [lpos]
        mov     bx,[lpos]
        cmp     bx,[line_len]
        pop     bx
        jb      .r
        call    png_line
.r:     ret

; the line in cur_base unfiltered against prev_base, its pixels averaged, the buffers swapped
png_line:
        pushad
        mov     [lpos],0
        mov     si,[cur_base]
        mov     di,[prev_base]
        mov     al,[es:si]
        inc     si                      ; the first byte
        inc     di
        mov     cx,[line_bytes]
        mov     dx,[png_bpp]
        cmp     al,0
        je      .pixels
        cmp     al,1
        je      .sub
        cmp     al,2
        je      .up
        cmp     al,3
        je      .avg
        cmp     al,4
        je      .paeth
        mov     ax,s_png_bad
        jmp     dec_fail
.sub:   mov     bx,si
        add     bx,dx
        sub     cx,dx
        jbe     .pixels
@@:     mov     al,[es:si]
        add     [es:bx],al
        inc     si
        inc     bx
        loop    @b
        jmp     .pixels
.up:    mov     al,[es:di]
        add     [es:si],al
        inc     si
        inc     di
        loop    .up
        jmp     .pixels
.avg:   xor     bx,bx                   ; index
.al:    movzx   ax,byte [es:di+bx]      ; up
        cmp     bx,dx
        jb      @f
        push    si
        add     si,bx
        sub     si,dx
        movzx   bp,byte [es:si]         ; left
        pop     si
        add     ax,bp
@@:     shr     ax,1
        add     [es:si+bx],al
        inc     bx
        cmp     bx,cx
        jb      .al
        jmp     .pixels
.paeth: xor     bx,bx
.pl:    cmp     bx,dx
        jae     .pfull
        mov     al,[es:di+bx]           ; no left: the one above
        add     [es:si+bx],al
        jmp     .pnext
.pfull: push    cx
        push    si
        push    di
        add     si,bx
        sub     si,dx
        add     di,bx
        movzx   ax,byte [es:si]         ; a, left
        movzx   bp,byte [es:di]         ; b, above
        sub     di,dx
        movzx   cx,byte [es:di]         ; c, above left
        pop     di
        pop     si
        ; pa = |b - c|, pb = |a - c|, pc = |a + b - 2c|
        push    dx
        mov     dx,bp
        sub     dx,cx
        jns     @f
        neg     dx
@@:     mov     [pa],dx
        mov     dx,ax
        sub     dx,cx
        jns     @f
        neg     dx
@@:     mov     [pb],dx
        mov     dx,ax
        add     dx,bp
        sub     dx,cx
        sub     dx,cx
        jns     @f
        neg     dx
@@:     mov     [pc],dx
        pop     dx
        mov     [pv_t],ax               ; a
        mov     ax,[pa]
        cmp     ax,[pb]
        ja      .notA
        cmp     ax,[pc]
        ja      .notA
        mov     ax,[pv_t]
        jmp     .pred
.notA:  mov     ax,[pb]
        cmp     ax,[pc]
        ja      .isC
        mov     ax,bp
        jmp     .pred
.isC:   mov     ax,cx
.pred:  pop     cx
        add     [es:si+bx],al
.pnext: inc     bx
        cmp     bx,cx
        jb      .pl
.pixels:
        ; this row, if it is sampled
        mov     eax,[pass_row]
        xor     edx,edx
        div     dword [pstep_y]
        test    edx,edx
        jnz     .swap
        mov     eax,[pass_row]
        mul     dword [pass_dy]
        add     eax,[pass_y0]
        call    acc_row
        call    png_pixels
.swap:  mov     ax,[cur_base]
        xchg    ax,[prev_base]
        mov     [cur_base],ax
        inc     dword [pass_row]
        test    byte [pass_row],31      ; now and then, whether a key is waiting
        jnz     @f
        call    key_ready
        jz      @f
        xor     ax,ax
        jmp     dec_fail
@@:     mov     eax,[pass_row]
        cmp     eax,[pass_h]
        jb      .out
        call    png_next_pass
.out:   popad
        ret

; the sampled pixels of the unfiltered line at cur_base, into acc
png_pixels:
        xor     ebp,ebp                 ; i, in the pass
.l:     cmp     ebp,[pass_w]
        jae     .r
        call    png_pixel
        mov     eax,ebp
        mul     dword [pass_dx]
        add     eax,[pass_x0]
        call    acc_add
        add     ebp,[pstep_x]
        jmp     .l
.r:     ret

; pixel ebp of the line at cur_base → px_r, px_g, px_b, px_a
png_pixel:
        mov     si,[cur_base]
        inc     si
        mov     [px_a],255
        cmp     [png_depth],8
        jb      .small
        ; whole bytes: offset i * bytes a pixel, the high byte of each 16-bit sample
        mov     eax,ebp
        movzx   ecx,[png_bpp]
        mul     ecx
        add     si,ax
        mov     bx,1
        cmp     [png_depth],16
        jne     @f
        mov     bx,2
@@:     mov     al,[png_ctype]
        cmp     al,2
        je      .rgb
        cmp     al,6
        je      .rgb
        cmp     al,3
        je      .pal8
        mov     al,[es:si]              ; gray, gray + alpha
        mov     [px_r],al
        mov     [px_g],al
        mov     [px_b],al
        cmp     [png_ctype],4
        jne     .r
        mov     al,[es:si+bx]
        mov     [px_a],al
        ret
.rgb:   mov     al,[es:si]              ; samples bx bytes apart
        mov     [px_r],al
        add     si,bx
        mov     al,[es:si]
        mov     [px_g],al
        add     si,bx
        mov     al,[es:si]
        mov     [px_b],al
        cmp     [png_ctype],6
        jne     .r
        add     si,bx
        mov     al,[es:si]
        mov     [px_a],al
.r:     ret
.pal8:  movzx   bx,byte [es:si]
        jmp     .index
.small: ; 1, 2 or 4 bits: gray or a palette index
        mov     eax,ebp
        movzx   ecx,[png_depth]
        mul     ecx                     ; the bit offset
        mov     ecx,eax
        shr     eax,3
        add     si,ax
        mov     al,[es:si]
        and     cl,7
        add     cl,[png_depth]
        neg     cl
        add     cl,8                    ; shift = 8 - depth - (offset & 7)
        shr     al,cl
        mov     cl,[png_depth]
        mov     ah,1
        shl     ah,cl
        dec     ah
        and     al,ah                   ; the value
        cmp     [png_ctype],3
        jne     .gray
        movzx   bx,al
        jmp     .index
.gray:  mov     cl,255
        cmp     [png_depth],1
        je      @f
        mov     cl,85
        cmp     [png_depth],2
        je      @f
        mov     cl,17
@@:     mul     cl
        mov     [px_r],al
        mov     [px_g],al
        mov     [px_b],al
        ret
.index: mov     al,[pal_a+bx]
        mov     [px_a],al
        imul    bx,bx,3
        mov     al,[pal+bx]
        mov     [px_r],al
        mov     al,[pal+bx+1]
        mov     [px_g],al
        mov     al,[pal+bx+2]
        mov     [px_b],al
        ret

; ---------------------------------------------------------------------------------------------
; inflate (RFC 1951), from the IDAT chunks

; al ← the next byte of the zlib stream, across IDAT chunks
idat_byte:
        cmp     dword [idat_left],0
        jne     .have
        push    eax
        push    ecx
        mov     ecx,4                   ; the CRC
        call    rd_skip
        call    rd_be32
        mov     [idat_left],eax
        call    rd_be32
        cmp     eax,'TADI'
        pop     ecx
        pop     eax
        je      idat_byte
        mov     ax,s_truncated
        jmp     dec_fail
.have:  dec     dword [idat_left]
        jmp     rd_byte

; the next bit → eax
zbit:
        cmp     [bitcnt],0
        jne     @f
        push    ax
        call    idat_byte
        mov     byte [bitbuf],al
        pop     ax
        mov     [bitcnt],8
@@:     mov     eax,[bitbuf]
        shr     dword [bitbuf],1
        dec     [bitcnt]
        and     eax,1
        ret

; cl bits (up to 16), least significant first → eax
zbits:
        push    edx
        push    cx
.need:  cmp     [bitcnt],cl
        jae     .have
        call    idat_byte
        movzx   edx,al
        push    cx
        mov     cl,[bitcnt]
        shl     edx,cl
        pop     cx
        or      [bitbuf],edx
        add     [bitcnt],8
        jmp     .need
.have:  mov     eax,[bitbuf]
        mov     edx,1
        shl     edx,cl
        dec     edx
        and     eax,edx
        shr     dword [bitbuf],cl
        sub     [bitcnt],cl
        pop     cx
        pop     edx
        ret

; si = a code (16 counts, then symbols, words) → ax = the next symbol (puff's decode)
zdecode:
        push    ebx
        push    ecx
        push    edx
        push    edi
        push    esi
        movzx   esi,si
        xor     edx,edx                 ; code
        xor     ebx,ebx                 ; first
        xor     edi,edi                 ; index
        mov     ecx,1
.l:     call    zbit
        or      edx,eax
        movzx   eax,word [esi+ecx*2]    ; how many codes this long
        add     ebx,eax
        cmp     edx,ebx
        jb      .found
        add     edi,eax
        shl     ebx,1
        shl     edx,1
        inc     ecx
        cmp     ecx,15
        jbe     .l
        mov     ax,s_png_bad
        jmp     dec_fail
.found: sub     edx,ebx
        add     edx,eax
        add     edx,edi
        movzx   eax,word [esi+32+edx*2]
        pop     esi
        pop     edi
        pop     edx
        pop     ecx
        pop     ebx
        ret

; si = a code, bx = code lengths (bytes), cx = how many: the counts and symbols (puff's construct)
zbuild:
        pushad
        push    es
        push    ds
        pop     es
        movzx   esi,si
        mov     di,si
        push    cx
        mov     cx,16
        xor     ax,ax
        rep     stosw
        pop     cx
        movzx   ecx,cx
        movzx   ebx,bx
        xor     edx,edx
.count: cmp     edx,ecx
        jae     .offs
        movzx   eax,byte [ebx+edx]
        inc     word [esi+eax*2]
        inc     edx
        jmp     .count
.offs:  mov     word [zoffs+2],0
        mov     eax,1
.o:     mov     dx,[zoffs+eax*2]
        add     dx,[esi+eax*2]
        mov     [zoffs+eax*2+2],dx
        inc     eax
        cmp     eax,15
        jb      .o
        xor     edx,edx
.sym:   cmp     edx,ecx
        jae     .done
        movzx   eax,byte [ebx+edx]
        test    eax,eax
        jz      .sn
        movzx   edi,word [zoffs+eax*2]
        inc     word [zoffs+eax*2]
        mov     [esi+32+edi*2],dx
.sn:    inc     edx
        jmp     .sym
.done:  pop     es
        popad
        ret

inflate:
        call    idat_byte               ; CMF: deflate, a window no bigger than 32 KiB
        and     al,0Fh
        cmp     al,8
        jne     .bad
        call    idat_byte               ; FLG
        mov     dword [bitbuf],0
        mov     [bitcnt],0
        mov     [wpos],0
.block: mov     cl,1
        call    zbits
        mov     [zlast],al
        mov     cl,2
        call    zbits
        cmp     al,0
        je      .stored
        cmp     al,1
        je      .fixed
        cmp     al,2
        je      .dynamic
.bad:   mov     ax,s_png_bad
        jmp     dec_fail
.next:  cmp     [png_done],0            ; all the pixels: the rest does not matter
        jne     .r
        cmp     [zlast],0
        je      .block
.r:     ret
.stored:
        mov     dword [bitbuf],0        ; to the byte boundary
        mov     [bitcnt],0
        call    idat_byte
        mov     cl,al
        call    idat_byte
        mov     ch,al
        call    idat_byte               ; NLEN
        call    idat_byte
        jcxz    .next
@@:     call    idat_byte
        call    out_byte
        loop    @b
        jmp     .next
.fixed: push    es                      ; the lengths are in DS; ES is the line buffers
        push    ds
        pop     es
        mov     di,zlens
        mov     al,8
        mov     cx,144
        rep     stosb
        mov     al,9
        mov     cx,112
        rep     stosb
        mov     al,7
        mov     cx,24
        rep     stosb
        mov     al,8
        mov     cx,8
        rep     stosb
        mov     si,lencode
        mov     bx,zlens
        mov     cx,288
        call    zbuild
        mov     di,zlens
        mov     al,5
        mov     cx,30
        rep     stosb
        mov     si,distcode
        mov     bx,zlens
        mov     cx,30
        call    zbuild
        pop     es
        jmp     .codes
.dynamic:
        mov     cl,5
        call    zbits
        add     ax,257
        mov     [znlen],ax
        mov     cl,5
        call    zbits
        inc     ax
        mov     [zndist],ax
        mov     cl,4
        call    zbits
        add     ax,4
        mov     [zncode],ax
        cmp     [znlen],286
        ja      .bad
        cmp     [zndist],30
        ja      .bad
        push    es
        push    ds
        pop     es
        mov     di,zlens
        xor     al,al
        mov     cx,19
        rep     stosb
        pop     es
        xor     si,si
.cl:    cmp     si,[zncode]
        jae     .clb
        mov     cl,3
        call    zbits
        movzx   bx,[zorder+si]
        mov     [zlens+bx],al
        inc     si
        jmp     .cl
.clb:   mov     si,lencode
        mov     bx,zlens
        mov     cx,19
        call    zbuild
        ; the literal/length and distance code lengths
        xor     di,di
        mov     cx,[znlen]
        add     cx,[zndist]
        mov     [zntotal],cx
.len:   cmp     di,[zntotal]
        jae     .built
        mov     si,lencode
        call    zdecode
        cmp     ax,16
        jae     .rep
        mov     [zlens+di],al
        inc     di
        jmp     .len
.rep:   xor     dl,dl                   ; the length repeated
        cmp     ax,16
        jne     .z
        test    di,di
        jz      .bad
        mov     dl,[zlens+di-1]
        mov     cl,2
        call    zbits
        add     ax,3
        jmp     .fill
.z:     cmp     ax,17
        jne     .z18
        mov     cl,3
        call    zbits
        add     ax,3
        jmp     .fill
.z18:   mov     cl,7
        call    zbits
        add     ax,11
.fill:  mov     cx,ax
        mov     ax,di
        add     ax,cx
        cmp     ax,[zntotal]
        ja      .bad
@@:     mov     [zlens+di],dl
        inc     di
        loop    @b
        jmp     .len
.built: mov     si,lencode
        mov     bx,zlens
        mov     cx,[znlen]
        call    zbuild
        mov     si,distcode
        mov     bx,zlens
        add     bx,[znlen]
        mov     cx,[zndist]
        call    zbuild
.codes: mov     si,lencode
        call    zdecode
        cmp     ax,256
        jb      .lit
        je      .next
        sub     ax,257
        cmp     ax,29
        jae     .bad
        mov     bx,ax
        mov     cl,[zlext+bx]
        shl     bx,1
        mov     dx,[zlbase+bx]
        call    zbits
        add     dx,ax                   ; the length
        push    dx
        mov     si,distcode
        call    zdecode
        cmp     ax,30
        jae     .bad2
        mov     bx,ax
        mov     cl,[zdext+bx]
        shl     bx,1
        mov     dx,[zdbase+bx]
        call    zbits
        add     dx,ax                   ; the distance
        pop     cx
        mov     bx,[wpos]
        sub     bx,dx
        and     bx,7FFFh
@@:     mov     al,[fs:bx]
        inc     bx
        and     bx,7FFFh
        call    out_byte
        cmp     [png_done],0
        jne     .r
        loop    @b
        jmp     .codes
.lit:   call    out_byte
        cmp     [png_done],0
        jne     .r
        jmp     .codes
.bad2:  pop     dx
        jmp     .bad

; al: into the window, then to the line
out_byte:
        push    bx
        mov     bx,[wpos]
        mov     [fs:bx],al
        inc     bx
        and     bx,7FFFh
        mov     [wpos],bx
        pop     bx
        jmp     png_put
