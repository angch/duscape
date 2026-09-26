; PNG.ASM, included by DUSCAPE.ASM: a PNG decoded into the preview's half blocks (PREVIEW.ASM).
;
; Every colour type and bit depth, Adam7 too, palette transparency (tRNS). Inflate follows Mark
; Adler's puff: canonical Huffman codes decoded a bit at a time, a 32 KiB window in winseg. Each
; line is unfiltered in lineseg (two line buffers, swapped) and its pixels averaged into acc,
; every step_x-th of every step_y-th row: a block pixel still gets dozens of them.
;
; Registers while decoding: ES = lineseg, DS = data; the window is read and written through a
; segment loaded for each access. 16-bit throughout: chunk lengths are two words, and the bit
; reader holds at most 16 bits, so reads wider than 8 bits are made in two.

PNG_MAXLINE     = 7FF0h         ; bytes in a line, filter byte included

png_decode:
        mov     [acc_mode],0
        mov     cx,8                    ; the signature, known already
@@:     call    rd_byte
        loop    @b
        mov     di,pal_a                ; opaque, unless tRNS says otherwise
        mov     al,255
        mov     cx,256
        rep     stosb
        mov     di,pal                  ; and black, where PLTE is short or missing
        xor     al,al
        mov     cx,768
        rep     stosb
        mov     [png_hdr],0
        mov     [png_plte],0
        mov     [png_trns],0
.chunk: call    rd_be32
        mov     [chunk_len],ax
        mov     [chunk_len+2],dx
        call    rd_be32                 ; the type, big-endian: 'IH','DR' as words
        cmp     dx,'HI'
        jne     .n1
        cmp     ax,'RD'
        je      .ihdr
.n1:    cmp     dx,'LP'
        jne     .n2
        cmp     ax,'ET'
        je      .plte
.n2:    cmp     dx,'Rt'
        jne     .n3
        cmp     ax,'SN'
        je      .trns
.n3:    cmp     dx,'DI'
        jne     .n4
        cmp     ax,'TA'
        je      .idat
.n4:    cmp     dx,'EI'
        jne     .skip
        cmp     ax,'DN'
        je      .noidat
.skip:  mov     cx,[chunk_len]
        mov     dx,[chunk_len+2]
        add     cx,4                    ; and the CRC
        adc     dx,0
        call    rd_skip
        jmp     .chunk
.noidat:
        mov     ax,s_png_noidat
        jmp     dec_fail
.ihdr:  call    rd_be32
        test    dx,dx
        jnz     .big
        mov     [img_w],ax
        call    rd_be32
        test    dx,dx
        jnz     .big
        mov     [img_h],ax
        cmp     [img_w],0
        je      .bad
        cmp     [img_h],0
        je      .bad
        call    pv_check_size
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
        mov     cx,[chunk_len]
        mov     dx,[chunk_len+2]
        sub     cx,13
        sbb     dx,0
        jb      .bad
        add     cx,4
        adc     dx,0
        call    rd_skip
        call    png_format
        mov     [png_hdr],1
        jmp     .chunk
.plte:  mov     [png_plte],1
        xor     bx,bx
@@:     call    .left
        jz      .crc
        call    rd_byte
        cmp     bx,768
        jae     @b
        mov     [pal+bx],al
        inc     bx
        jmp     @b
.crc:   mov     cx,4
        xor     dx,dx
        call    rd_skip
        jmp     .chunk
.trns:  cmp     [png_ctype],0           ; gray and RGB: the one colour that is transparent
        je      .tkey
        cmp     [png_ctype],2
        je      .tkey
        cmp     [png_ctype],3
        jne     .skip
        xor     bx,bx
@@:     call    .left
        jz      .crc
        call    rd_byte
        cmp     bx,256
        jae     @b
        mov     [pal_a+bx],al
        inc     bx
        jmp     @b
.tkey:  xor     bx,bx                   ; up to three 16-bit samples, as they are stored
@@:     call    .left
        jz      .tkeyd
        call    rd_byte
        cmp     bx,6
        jae     @b
        mov     [png_key+bx],al
        inc     bx
        jmp     @b
.tkeyd: mov     [png_trns],1
        jmp     .crc
.idat:  cmp     [png_hdr],0
        je      .bad
        cmp     [png_ctype],3           ; a palette image needs its palette
        jne     @f
        cmp     [png_plte],0
        je      .bad
@@:
        mov     ax,[chunk_len]
        mov     [idat_left],ax
        mov     ax,[chunk_len+2]
        mov     [idat_left+2],ax
        call    acc_setup
        mov     es,[lineseg]
        call    png_first_pass
        call    inflate
        cmp     [png_done],0
        je      .trunc
        clc
        ret
.trunc: mov     ax,s_truncated
        jmp     dec_fail
.big:   mov     ax,s_too_large
        jmp     dec_fail
.bad:   mov     ax,s_png_bad
        jmp     dec_fail
; ZF=1 when the chunk has no bytes left; else one less
.left:  mov     ax,[chunk_len]
        or      ax,[chunk_len+2]
        jz      @f
        sub     word [chunk_len],1
        sbb     word [chunk_len+2],0
        or      sp,sp                   ; ZF=0
@@:     ret

; the header checked, the description written, the pixel size worked out
png_format:
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
        cmp     al,8                    ; 8 and 16 for every type (not 16 with a palette)
        je      .d
        cmp     al,16
        jne     .small
        cmp     [png_ctype],3
        je      .bad
        jmp     .d
.small: cmp     [png_ctype],0           ; 1, 2 and 4 for gray and palettes only
        je      @f
        cmp     [png_ctype],3
        jne     .bad
@@:     cmp     al,1
        je      .d
        cmp     al,2
        je      .d
        cmp     al,4
        jne     .bad
.d:     mul     cl
        mov     [png_bpx],ax            ; bits a pixel
        add     ax,7
        shr     ax,1
        shr     ax,1
        shr     ax,1
        mov     [png_bpp],ax            ; bytes a pixel, at least one (for the filters)
        ; "PNG 1920x1080"
        push    es
        push    ds
        pop     es
        mov     di,pv_desc
        mov     si,s_png
        call    append_z
        mov     ax,[img_w]
        xor     dx,dx
        call    u32dec
        mov     al,'x'
        stosb
        mov     ax,[img_h]
        xor     dx,dx
        call    u32dec
        mov     byte [di],0
        pop     es
        ret
.bad:   mov     ax,s_png_bad
        jmp     dec_fail

; Adam7: x0, y0, dx, dy of each pass
adam7   db 0,0,8,8, 4,0,8,8, 0,4,4,8, 2,0,4,4, 0,2,2,4, 1,0,2,2, 0,1,1,2

png_first_pass:
        mov     [png_done],0
        mov     [png_pass],-1
        cmp     [png_interlace],0
        jne     png_next_pass
        mov     [pass_x0],0
        mov     [pass_y0],0
        mov     [pass_dx],1
        mov     [pass_dy],1
        mov     ax,[img_w]
        mov     [pass_w],ax
        mov     ax,[img_h]
        mov     [pass_h],ax
        mov     [png_pass],7
        jmp     png_start_pass

; the next pass that has pixels, or done
png_next_pass:
        inc     [png_pass]
        cmp     [png_pass],7
        jae     .done
        mov     bl,[png_pass]
        xor     bh,bh
        shl     bx,1
        shl     bx,1
        mov     al,[cs:adam7+bx]
        xor     ah,ah
        mov     [pass_x0],ax
        mov     al,[cs:adam7+bx+1]
        mov     [pass_y0],ax
        mov     al,[cs:adam7+bx+2]
        mov     [pass_dx],ax
        mov     al,[cs:adam7+bx+3]
        mov     [pass_dy],ax
        ; ceil((W - x0) / dx), ceil((H - y0) / dy)
        mov     ax,[img_w]
        sub     ax,[pass_x0]
        jbe     png_next_pass
        add     ax,[pass_dx]
        dec     ax
        xor     dx,dx
        div     word [pass_dx]
        mov     [pass_w],ax
        mov     ax,[img_h]
        sub     ax,[pass_y0]
        jbe     png_next_pass
        add     ax,[pass_dy]
        dec     ax
        xor     dx,dx
        div     word [pass_dy]
        mov     [pass_h],ax
        jmp     png_start_pass
.done:  mov     [png_done],1
        ret

png_start_pass:
        mov     ax,[pass_w]             ; ceil(w × bits / 8) bytes, and the filter byte
        mul     word [png_bpx]
        add     ax,7
        adc     dx,0
        mov     cx,3
@@:     shr     dx,1
        rcr     ax,1
        loop    @b
        test    dx,dx
        jnz     .wide
        cmp     ax,PNG_MAXLINE
        jae     .wide
        mov     [line_bytes],ax
        inc     ax
        mov     [line_len],ax
        mov     [pass_row],0
        mov     [lpos],0
        mov     [cur_base],8000h
        mov     [prev_base],0
        xor     di,di                   ; the line above the first is zeros
        mov     cx,[line_len]
        xor     al,al
        rep     stosb
        ; sampling in the pass's own pixels
        mov     ax,[step_x]
        xor     dx,dx
        div     word [pass_dx]
        test    ax,ax
        jnz     @f
        inc     ax
@@:     mov     [pstep_x],ax
        mov     ax,[step_y]
        xor     dx,dx
        div     word [pass_dy]
        test    ax,ax
        jnz     @f
        inc     ax
@@:     mov     [pstep_y],ax
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
        pusha
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
.al:    mov     al,[es:di+bx]           ; up
        xor     ah,ah
        cmp     bx,dx
        jb      @f
        push    si
        add     si,bx
        sub     si,dx
        mov     bp,ax
        mov     al,[es:si]              ; left
        xor     ah,ah
        add     ax,bp
        pop     si
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
        mov     al,[es:si]              ; a, left
        xor     ah,ah
        mov     bp,ax
        mov     al,[es:di]              ; b, above
        mov     [pv_b],ax
        sub     di,dx
        mov     al,[es:di]              ; c, above left
        mov     cx,ax
        pop     di
        pop     si
        mov     ax,bp
        ; pa = |b - c|, pb = |a - c|, pc = |a + b - 2c|
        push    dx
        mov     dx,[pv_b]
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
        add     dx,[pv_b]
        sub     dx,cx
        sub     dx,cx
        jns     @f
        neg     dx
@@:     mov     [pc],dx
        pop     dx
        mov     ax,[pa]
        cmp     ax,[pb]
        ja      .notA
        cmp     ax,[pc]
        ja      .notA
        mov     ax,bp
        jmp     .pred
.notA:  mov     ax,[pb]
        cmp     ax,[pc]
        ja      .isC
        mov     ax,[pv_b]
        jmp     .pred
.isC:   mov     ax,cx
.pred:  pop     cx
        add     [es:si+bx],al
.pnext: inc     bx
        cmp     bx,cx
        jb      .pl
.pixels:
        ; this row, if it is sampled
        mov     ax,[pass_row]
        xor     dx,dx
        div     word [pstep_y]
        test    dx,dx
        jnz     .swap
        mov     ax,[pass_row]
        mul     word [pass_dy]
        add     ax,[pass_y0]
        call    acc_row
        call    png_pixels
.swap:  mov     ax,[cur_base]
        xchg    ax,[prev_base]
        mov     [cur_base],ax
        inc     [pass_row]
        test    byte [pass_row],31      ; now and then, whether a key is waiting
        jnz     @f
        call    key_ready
        jz      @f
        xor     ax,ax
        jmp     dec_fail
@@:     mov     ax,[pass_row]
        cmp     ax,[pass_h]
        jb      .out
        call    png_next_pass
.out:   popa
        ret

; the sampled pixels of the unfiltered line at cur_base, into acc
png_pixels:
        xor     bp,bp                   ; i, in the pass
.l:     cmp     bp,[pass_w]
        jae     .r
        call    png_pixel
        mov     ax,bp
        mul     word [pass_dx]
        add     ax,[pass_x0]
        call    acc_add
        add     bp,[pstep_x]
        jc      .r
        jmp     .l
.r:     ret

; pixel bp of the line at cur_base → px_r, px_g, px_b, px_a
png_pixel:
        mov     si,[cur_base]
        inc     si
        mov     [px_a],255
        cmp     [png_depth],8
        jb      .small
        ; whole bytes: offset i × bytes a pixel (within the line, so 16 bits), the high byte of
        ; each 16-bit sample
        mov     ax,bp
        mul     word [png_bpp]
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
        cmp     [png_ctype],0
        jne     @f
        xor     di,di                   ; gray: its key
        call    .key
        jnc     .r
        mov     [px_a],0
        ret
@@:     cmp     [png_ctype],4
        jne     .r
        mov     al,[es:si+bx]
        mov     [px_a],al
        ret
.rgb:   cmp     [png_ctype],2
        jne     .rgbv
        push    si                      ; RGB: all three samples must be the key's
        xor     di,di
        call    .key
        jnc     @f
        add     si,bx
        mov     di,2
        call    .key
        jnc     @f
        add     si,bx
        mov     di,4
        call    .key
        jnc     @f
        mov     [px_a],0
@@:     pop     si
.rgbv:  mov     al,[es:si]              ; samples bx bytes apart
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
.pal8:  mov     bl,[es:si]
        xor     bh,bh
        jmp     .index
.small: ; 1, 2 or 4 bits: gray or a palette index; the bit offset is 32 bits
        mov     ax,bp
        mov     cl,[png_depth]
        xor     ch,ch
        mul     cx
        mov     cx,ax
        mov     bx,3                    ; the byte: offset / 8
@@:     shr     dx,1
        rcr     ax,1
        dec     bx
        jnz     @b
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
.pidx:  mov     bl,al
        xor     bh,bh
        jmp     .index
.gray:  cmp     [png_trns],0            ; the key is the value before it is scaled
        je      .scale
        cmp     byte [png_key],0
        jne     .scale
        cmp     al,[png_key+1]
        jne     .scale
        mov     [px_a],0
.scale: mov     cl,255
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
; es:si = a raw sample (8 or 16 bits), di = which of the key's (0, 2, 4) → CF=1 if tRNS gave
; this value
.key:   cmp     [png_trns],0
        je      .nokey
        cmp     [png_depth],16
        jne     .k8
        mov     al,[es:si]
        cmp     al,[png_key+di]
        jne     .nokey
        mov     al,[es:si+1]
        cmp     al,[png_key+di+1]
        jne     .nokey
        stc
        ret
.k8:    cmp     byte [png_key+di],0
        jne     .nokey
        mov     al,[es:si]
        cmp     al,[png_key+di+1]
        jne     .nokey
        stc
        ret
.nokey:   clc
        ret
.index: mov     al,[pal_a+bx]
        mov     [px_a],al
        mov     ax,bx
        shl     bx,1
        add     bx,ax
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
        mov     ax,[idat_left]
        or      ax,[idat_left+2]
        jnz     .have
        push    cx
        push    dx
        mov     cx,4                    ; the CRC
        xor     dx,dx
        call    rd_skip
        call    rd_be32
        mov     [idat_left],ax
        mov     [idat_left+2],dx
        call    rd_be32
        cmp     dx,'DI'
        jne     .trunc
        cmp     ax,'TA'
        jne     .trunc
        pop     dx
        pop     cx
        jmp     idat_byte
.trunc: mov     ax,s_truncated
        jmp     dec_fail
.have:  sub     word [idat_left],1
        sbb     word [idat_left+2],0
        jmp     rd_byte

; the next bit → ax
zbit:
        cmp     [bitcnt],0
        jne     @f
        call    idat_byte
        xor     ah,ah
        mov     [bitbuf],ax
        mov     [bitcnt],8
@@:     mov     ax,[bitbuf]
        shr     [bitbuf],1
        dec     [bitcnt]
        and     ax,1
        ret

; cl bits (up to 16), least significant first → ax
zbits:
        cmp     cl,8
        jbe     zbits8
        push    cx                      ; wider: the low 8, then the rest above them
        push    dx
        mov     ch,cl
        mov     cl,8
        call    zbits8
        mov     dx,ax
        mov     cl,ch
        sub     cl,8
        call    zbits8
        mov     ah,al
        mov     al,dl
        pop     dx
        pop     cx
        ret
; cl bits, at most 8: the buffer holds up to 15
zbits8:
        push    dx
        push    cx
.need:  cmp     [bitcnt],cl
        jae     .have
        call    idat_byte
        xor     ah,ah
        push    cx
        mov     cl,[bitcnt]
        shl     ax,cl
        pop     cx
        or      [bitbuf],ax
        add     [bitcnt],8
        jmp     .need
.have:  mov     ax,1
        shl     ax,cl
        dec     ax
        and     ax,[bitbuf]
        shr     [bitbuf],cl
        sub     [bitcnt],cl
        pop     cx
        pop     dx
        ret

; si = a code (16 counts, then symbols, words) → ax = the next symbol (puff's decode)
zdecode:
        push    bx
        push    cx
        push    dx
        push    di
        xor     dx,dx                   ; code
        xor     bx,bx                   ; first
        xor     di,di                   ; index
        mov     cx,2                    ; the count's offset: length × 2
.l:     call    zbit
        or      dx,ax
        push    si
        add     si,cx
        mov     ax,[si]                 ; how many codes this long
        pop     si
        add     bx,ax
        cmp     dx,bx
        jb      .found
        add     di,ax
        shl     bx,1
        shl     dx,1
        add     cx,2
        cmp     cx,30
        jbe     .l
        mov     ax,s_png_bad
        jmp     dec_fail
.found: sub     dx,bx
        add     dx,ax
        add     dx,di
        mov     bx,dx
        shl     bx,1
        mov     ax,[si+32+bx]
        pop     di
        pop     dx
        pop     cx
        pop     bx
        ret

; si = a code, bx = code lengths (bytes), cx = how many: the counts and symbols (puff's construct)
zbuild:
        pusha
        push    es
        push    ds
        pop     es
        mov     [zb_tab],si
        mov     di,si
        push    cx
        mov     cx,16
        xor     ax,ax
        rep     stosw
        pop     cx
        xor     dx,dx
.count: cmp     dx,cx
        jae     .offs
        mov     di,bx
        add     di,dx
        mov     al,[di]
        xor     ah,ah
        mov     di,ax
        shl     di,1
        add     di,[zb_tab]
        inc     word [di]
        inc     dx
        jmp     .count
.offs:  mov     word [zoffs+2],0
        mov     di,2
.o:     mov     ax,[zoffs+di]
        mov     si,di
        add     si,[zb_tab]
        add     ax,[si]
        mov     [zoffs+di+2],ax
        add     di,2
        cmp     di,30
        jb      .o
        xor     dx,dx
.sym:   cmp     dx,cx
        jae     .done
        mov     di,bx
        add     di,dx
        mov     al,[di]
        xor     ah,ah
        test    ax,ax
        jz      .sn
        mov     di,ax
        shl     di,1
        mov     ax,[zoffs+di]
        inc     word [zoffs+di]
        mov     di,ax
        shl     di,1
        add     di,[zb_tab]
        mov     [di+32],dx
.sn:    inc     dx
        jmp     .sym
.done:  pop     es
        popa
        ret

inflate:
        call    idat_byte               ; CMF: deflate, a window no bigger than 32 KiB
        and     al,0Fh
        cmp     al,8
        jne     .bad
        call    idat_byte               ; FLG
        mov     [bitbuf],0
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
        mov     [bitbuf],0              ; to the byte boundary
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
        mov     bl,[zorder+si]
        xor     bh,bh
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
@@:     push    es
        mov     es,[winseg]
        mov     al,[es:bx]
        pop     es
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
        push    es
        mov     bx,[wpos]
        mov     es,[winseg]
        mov     [es:bx],al
        pop     es
        inc     bx
        and     bx,7FFFh
        mov     [wpos],bx
        pop     bx
        jmp     png_put
