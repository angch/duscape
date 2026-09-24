; JPEG.ASM, included by DISKONAU.ASM: a JPEG decoded into the preview's half blocks
; (PREVIEW.ASM).
;
; A block pixel of the preview covers many 8x8 blocks of the image, and the mean of a block is
; its DC coefficient alone: DC x the quantiser / 8 + 128. So no inverse DCT: the Huffman codes
; are decoded (the AC ones only to be skipped) and each block's mean is averaged into acc, as Y,
; Cb and Cr, converted to RGB at the end; only every jstep-th block each way is averaged, about
; eight to a block pixel a side. Baseline and extended sequential JPEGs, and progressive ones
; from their first DC scans (the AC scans are skipped). Grayscale or YCbCr, any sampling
; factors, restart intervals. Not lossless, arithmetic-coded, 12-bit or CMYK.
;
; A scan ends at the first marker that is not a restart, so data cut short cannot keep it going,
; and the keyboard is looked at every row of blocks.

J_TABLE         = 32+256        ; a Huffman table: 16 word counts, then up to 256 symbols

jpeg_decode:
        call    rd_be16                 ; FFD8, known already
        mov     [jsof],0
        mov     [jri],0
        mov     [jdone],0
        mov     [jmarker],0
        mov     [jhdef],0               ; the tables defined so far, a bit each
        mov     [jqdef],0
        mov     [acc_mode],1
.marker:
        call    jnext_marker
        cmp     al,0D8h
        je      .marker
        cmp     al,0D9h
        je      .eoi
        cmp     al,0C0h
        je      .sof
        cmp     al,0C1h
        je      .sof
        cmp     al,0C2h
        je      .sofp
        cmp     al,0C4h
        je      .dht
        cmp     al,0DBh
        je      .dqt
        cmp     al,0DDh
        je      .dri
        cmp     al,0DAh
        je      .sos
        cmp     al,0D0h
        jb      @f
        cmp     al,0D7h
        jbe     .marker                 ; a stray restart marker
@@:     cmp     al,0C3h
        jb      .skip
        cmp     al,0CFh
        ja      .skip
        mov     ax,s_jpeg_kind          ; lossless, hierarchical or arithmetic-coded
        jmp     dec_fail
.skip:  call    .seglen
        call    rd_skip
        jmp     .marker
.eoi:   cmp     [jdone],0
        je      .noscan
        clc                             ; what the scans gave, if not everything
        ret
.noscan:
        mov     ax,s_jpeg_noscan
        jmp     dec_fail
.bad:   mov     ax,s_jpeg_bad
        jmp     dec_fail
; dx:cx ← a segment's length, less its own two bytes
.seglen:
        call    rd_be16
        sub     ax,2
        jb      .bad
        mov     cx,ax
        xor     dx,dx
        ret

.sofp:  mov     [jprog],1
        jmp     .frame
.sof:   mov     [jprog],0
.frame: call    rd_be16                 ; length
        call    rd_byte
        cmp     al,8
        jne     .kind
        call    rd_be16
        mov     [img_h],ax
        call    rd_be16
        mov     [img_w],ax
        cmp     [img_h],0               ; the height in a DNL marker
        je      .kind
        call    pv_check_size
        call    rd_byte
        mov     [jnf],al
        cmp     al,1
        je      @f
        cmp     al,3
        jne     .cmyk
@@:     mov     [jhmax],1
        mov     [jvmax],1
        xor     bx,bx
.comp:  cmp     bl,[jnf]
        jae     .comps
        call    rd_byte
        mov     [jc_id+bx],al
        call    rd_byte
        mov     ah,al
        shr     ah,1
        shr     ah,1
        shr     ah,1
        shr     ah,1
        and     al,0Fh
        jz      .bad
        test    ah,ah
        jz      .bad
        cmp     ah,4
        ja      .bad
        cmp     al,4
        ja      .bad
        mov     [jc_h+bx],ah
        mov     [jc_v+bx],al
        cmp     ah,[jhmax]
        jbe     @f
        mov     [jhmax],ah
@@:     cmp     al,[jvmax]
        jbe     @f
        mov     [jvmax],al
@@:     call    rd_byte
        and     al,3
        mov     [jc_tq+bx],al
        inc     bx
        jmp     .comp
.comps: mov     [jsof],1
        call    jpeg_describe
        call    acc_setup
        mov     [acc_mode],1
        call    jpeg_steps
        jmp     .marker
.kind:  mov     ax,s_jpeg_kind
        jmp     dec_fail
.cmyk:  mov     ax,s_jpeg_cmyk
        jmp     dec_fail

.dht:   call    .seglen
        mov     bp,cx                   ; what is left of the segment
.table: cmp     bp,17
        jb      .dhtend
        call    rd_byte
        mov     ah,al
        shr     ah,1
        shr     ah,1
        shr     ah,1
        shr     ah,1                    ; 0 DC, 1 AC
        cmp     ah,1
        ja      .bad
        and     al,3
        shl     ah,1
        shl     ah,1
        add     al,ah
        mov     [jslot],al
        xor     dx,dx                   ; how many symbols
        xor     bx,bx
.cnt:   call    rd_byte                 ; the counts, kept aside until they are known good
        xor     ah,ah
        mov     [jtmp+bx],ax
        add     dx,ax
        add     bx,2
        cmp     bx,32
        jb      .cnt
        sub     bp,17
        cmp     dx,256
        ja      .bad
        cmp     dx,bp
        ja      .bad
        sub     bp,dx
        mov     cx,dx                   ; the symbols (mul takes dx)
        mov     al,[jslot]
        xor     ah,ah
        mov     di,J_TABLE
        mul     di
        mov     di,ax
        add     di,jhuff
        xor     bx,bx
@@:     mov     ax,[jtmp+bx]
        mov     [di+bx],ax
        add     bx,2
        cmp     bx,32
        jb      @b
        xor     bx,bx
        jcxz    .def
@@:     call    rd_byte
        mov     [di+32+bx],al
        inc     bx
        loop    @b
.def:   mov     cl,[jslot]
        mov     al,1
        shl     al,cl
        or      [jhdef],al
        jmp     .table
.dhtend:
        mov     cx,bp
        xor     dx,dx
        call    rd_skip
        jmp     .marker

.dqt:   call    .seglen
        mov     bp,cx
.q:     cmp     bp,65
        jb      .dqtend
        call    rd_byte
        mov     bl,al
        and     bx,3
        mov     cl,bl
        mov     ah,1
        shl     ah,cl
        or      [jqdef],ah
        shl     bx,1
        shr     al,1
        shr     al,1
        shr     al,1
        shr     al,1
        mov     cx,63
        sub     bp,65
        test    al,al
        jz      .q8
        call    rd_be16                 ; 16-bit values, at most 32767 as libjpeg has them (the
        cmp     ax,7FFFh                ; mean is taken signed)
        jbe     @f
        mov     ax,7FFFh
@@:     mov     [jq+bx],ax
        mov     cx,126
        sub     bp,64
        jb      .bad
        jmp     .qrest
.q8:    call    rd_byte
        xor     ah,ah
        mov     [jq+bx],ax
.qrest: xor     dx,dx
        call    rd_skip                 ; only the DC one matters
        jmp     .q
.dqtend:
        mov     cx,bp
        xor     dx,dx
        call    rd_skip
        jmp     .marker

.dri:   call    rd_be16
        call    rd_be16
        mov     [jri],ax
        jmp     .marker

.sos:   cmp     [jsof],0
        je      .bad
        call    rd_be16
        call    rd_byte
        mov     byte [jnscan],al        ; a word, compared with si
        mov     byte [jnscan+1],0
        cmp     al,1
        jb      .bad
        cmp     al,[jnf]
        ja      .bad
        xor     si,si
.sc:    cmp     si,[jnscan]
        jae     .scd
        call    rd_byte                 ; the component, by id
        xor     bx,bx
@@:     cmp     bl,[jnf]
        jae     .bad
        cmp     [jc_id+bx],al
        je      @f
        inc     bx
        jmp     @b
@@:     mov     [jsc+si],bl
        call    rd_byte
        mov     ah,al
        shr     ah,1
        shr     ah,1
        shr     ah,1
        shr     ah,1
        and     ax,0303h
        mov     [jc_td+bx],ah
        mov     [jc_ta+bx],al
        inc     si
        jmp     .sc
.scd:   call    rd_byte
        mov     [jss],al
        call    rd_byte
        mov     [jse],al
        call    rd_byte
        mov     ah,al
        shr     ah,1
        shr     ah,1
        shr     ah,1
        shr     ah,1
        and     al,0Fh
        mov     [jah],ah
        mov     [jal],al
        cmp     [jprog],0
        je      .decode
        cmp     [jss],0                 ; progressive: only the first DC scans
        jne     .pass
        cmp     [jah],0
        jne     .pass
.decode:
        call    jscan_tables            ; every table the scan names must be there
        call    jscan
        xor     si,si
@@:     cmp     si,[jnscan]
        jae     @f
        mov     cl,[jsc+si]
        mov     al,1
        shl     al,cl
        or      [jdone],al
        inc     si
        jmp     @b
@@:     mov     cl,[jnf]                ; every component has its means: done
        mov     al,1
        shl     al,cl
        dec     al
        cmp     [jdone],al
        jne     .marker
        clc
        ret
.pass:  call    jskip_scan
        jmp     .marker

; the scan's Huffman and quantisation tables defined, else corrupt
jscan_tables:
        xor     si,si
.l:     cmp     si,[jnscan]
        jae     .r
        mov     bl,[jsc+si]
        xor     bh,bh
        mov     cl,[jc_td+bx]           ; its DC table
        mov     al,1
        shl     al,cl
        test    [jhdef],al
        jz      .bad
        cmp     [jprog],0               ; and AC table, when the AC is decoded (skipped)
        jne     @f
        mov     cl,[jc_ta+bx]
        add     cl,4
        mov     al,1
        shl     al,cl
        test    [jhdef],al
        jz      .bad
@@:     mov     cl,[jc_tq+bx]
        mov     al,1
        shl     al,cl
        test    [jqdef],al
        jz      .bad
        inc     si
        jmp     .l
.r:     ret
.bad:   mov     ax,s_jpeg_bad
        jmp     dec_fail

; for each component, every how many blocks each way are averaged: about eight to a block
; pixel, so the sums stay small
jpeg_steps:
        xor     bx,bx
.l:     cmp     bl,[jnf]
        jae     .r
        mov     al,[jc_h+bx]
        mov     cx,[img_w]
        mov     dl,[jhmax]
        push    word [acc_cols]
        call    .one
        pop     cx
        mov     [jstep_x+bx],al
        mov     al,[jc_v+bx]
        mov     cx,[img_h]
        mov     dl,[jvmax]
        push    word [acc_hrows]
        call    .one
        pop     cx
        mov     [jstep_y+bx],al
        inc     bx
        jmp     .l
.r:     ret
; al = the sampling factor, cx = the side, dl = the largest factor, [sp+2] = the block pixels →
; al = max(1, the component's blocks / block pixels / 8), at most 255
.one:   push    bx
        push    bp
        mov     bp,sp
        mov     bl,dl                   ; the largest factor
        xor     bh,bh
        xor     ah,ah
        mul     cx                      ; the component's side: side × factor / max, in blocks
        div     bx
        add     ax,7
        shr     ax,1
        shr     ax,1
        shr     ax,1
        xor     dx,dx
        mov     cx,[bp+6]
        shl     cx,1
        shl     cx,1
        shl     cx,1
        div     cx
        test    ax,ax
        jnz     @f
        inc     ax
@@:     cmp     ax,255
        jbe     @f
        mov     ax,255
@@:     pop     bp
        pop     bx
        ret

; "JPEG 1920x1080"
jpeg_describe:
        push    es
        push    ds
        pop     es
        mov     di,pv_desc
        mov     si,s_jpeg
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

; al ← the next marker: the one the entropy decoder ran into, else the next in the file
jnext_marker:
        mov     al,[jmarker]
        test    al,al
        jz      .find
        mov     [jmarker],0
        ret
.find:  call    rd_byte
        cmp     al,0FFh
        jne     .find
@@:     call    rd_byte
        cmp     al,0FFh
        je      @b
        test    al,al                   ; FF00 is data
        jz      .find
        ret

; entropy-coded data not wanted: up to the next marker that is not a restart
jskip_scan:
        cmp     [jmarker],0
        jne     .r
.l:     call    rd_byte
        cmp     al,0FFh
        jne     .l
@@:     call    rd_byte
        cmp     al,0FFh
        je      @b
        test    al,al
        jz      .l
        cmp     al,0D0h
        jb      .m
        cmp     al,0D7h
        jbe     .l
.m:     mov     [jmarker],al
.r:     ret

; the next byte of entropy-coded data: FF00 is FF; at a marker, zeros from then on
jbyte:
        cmp     [jmarker],0
        jne     .zero
        call    rd_byte
        cmp     al,0FFh
        jne     .r
@@:     call    rd_byte
        cmp     al,0FFh
        je      @b
        test    al,al
        jz      .ff
        mov     [jmarker],al
.zero:  xor     al,al
        ret
.ff:    mov     al,0FFh
.r:     ret

; ZF=0 if the data has run into a marker that is not a restart: the scan is over
jat_end:
        mov     al,[jmarker]
        test    al,al
        jz      .r                      ; ZF=1: go on
        cmp     al,0D0h
        jb      .end
        cmp     al,0D7h
        jbe     .rst
.end:   or      sp,sp                   ; ZF=0
.r:     ret
.rst:   cmp     al,al                   ; ZF=1
        ret

; the next bit, most significant first → ax
jbit:
        cmp     [jcnt],0
        jne     @f
        call    jbyte
        mov     [jbits_buf],al
        mov     [jcnt],8
@@:     mov     al,[jbits_buf]
        shl     [jbits_buf],1
        dec     [jcnt]
        rol     al,1
        and     ax,1
        ret

; cl bits (up to 16) → ax
jbits:
        push    dx
        push    cx
        xor     dx,dx
        test    cl,cl
        jz      .r
@@:     call    jbit
        shl     dx,1
        or      dx,ax
        dec     cl
        jnz     @b
.r:     mov     ax,dx
        pop     cx
        pop     dx
        ret

; si = a table → al = the next symbol
jdecode:
        push    bx
        push    cx
        push    dx
        push    di
        xor     dx,dx                   ; code
        xor     bx,bx                   ; first
        xor     di,di                   ; index
        xor     cx,cx                   ; the count's offset: (length - 1) × 2
.l:     call    jbit
        or      dx,ax
        push    si
        add     si,cx
        mov     ax,[si]                 ; codes of this length
        pop     si
        add     bx,ax
        cmp     dx,bx
        jb      .found
        add     di,ax
        shl     bx,1
        shl     dx,1
        add     cx,2
        cmp     cx,32
        jb      .l
        mov     ax,s_jpeg_bad
        jmp     dec_fail
.found: sub     dx,bx
        add     dx,ax
        add     dx,di
        cmp     dx,256                  ; the counts were checked, but not against 256 codes
        jae     .bad
        mov     bx,dx
        mov     al,[si+32+bx]
        pop     di
        pop     dx
        pop     cx
        pop     bx
        ret
.bad:   mov     ax,s_jpeg_bad
        jmp     dec_fail

; one scan's blocks
jscan:
        mov     [jcnt],0
        mov     [jpred],0
        mov     [jpred+2],0
        mov     [jpred+4],0
        mov     ax,[jri]
        mov     [jtogo],ax
        cmp     [jnscan],1
        jne     .mcus
        ; one component: its own blocks, row by row
        mov     bl,[jsc]
        xor     bh,bh
        mov     [jcur],bl
        mov     al,[jc_h+bx]
        mov     cl,[jhmax]
        mov     dx,[img_w]
        call    .blocks
        mov     [jbw],ax
        mov     al,[jc_v+bx]
        mov     cl,[jvmax]
        mov     dx,[img_h]
        call    .blocks
        mov     [jbh],ax
        mov     [jby],0
.r1:    mov     ax,[jby]
        cmp     ax,[jbh]
        jae     .done
        mov     [jbx],0
.c1:    mov     ax,[jbx]
        cmp     ax,[jbw]
        jae     .n1
        call    jrestart
        call    jblock
        call    jat_end
        jnz     .done
        inc     [jbx]
        jmp     .c1
.n1:    inc     [jby]
        call    .key
        jmp     .r1
.mcus:  ; interleaved: MCU by MCU
        mov     al,[jhmax]
        xor     ah,ah
        shl     ax,1
        shl     ax,1
        shl     ax,1
        mov     cx,ax
        mov     ax,[img_w]
        add     ax,cx
        dec     ax
        xor     dx,dx
        div     cx
        mov     [jmcux],ax
        mov     al,[jvmax]
        xor     ah,ah
        shl     ax,1
        shl     ax,1
        shl     ax,1
        mov     cx,ax
        mov     ax,[img_h]
        add     ax,cx
        dec     ax
        xor     dx,dx
        div     cx
        mov     [jmcuy],ax
        mov     [jmy],0
.rm:    mov     ax,[jmy]
        cmp     ax,[jmcuy]
        jae     .done
        mov     [jmx],0
.cm:    mov     ax,[jmx]
        cmp     ax,[jmcux]
        jae     .nm
        call    jrestart
        xor     si,si
.k:     cmp     si,[jnscan]
        jae     .kd
        mov     bl,[jsc+si]
        xor     bh,bh
        mov     [jcur],bl
        mov     byte [jv],0
.v:     mov     al,[jv]
        cmp     al,[jc_v+bx]
        jae     .kn
        mov     byte [jh],0
.h:     mov     al,[jh]
        cmp     al,[jc_h+bx]
        jae     .vn
        mov     al,[jc_h+bx]            ; the block: mx*Hc + h, my*Vc + v
        xor     ah,ah
        mul     [jmx]
        mov     cl,[jh]
        xor     ch,ch
        add     ax,cx
        mov     [jbx],ax
        mov     al,[jc_v+bx]
        xor     ah,ah
        mul     [jmy]
        mov     cl,[jv]
        xor     ch,ch
        add     ax,cx
        mov     [jby],ax
        push    si
        push    bx
        call    jblock
        pop     bx
        pop     si
        inc     [jh]
        jmp     .h
.vn:    inc     [jv]
        jmp     .v
.kn:    inc     si
        jmp     .k
.kd:    call    jat_end
        jnz     .done
        inc     [jmx]
        jmp     .cm
.nm:    inc     [jmy]
        call    .key
        jmp     .rm
.done:  ret
; a key waiting stops the decode (for now)
.key:   call    key_ready
        jz      @f
        xor     ax,ax
        jmp     dec_fail
@@:     ret
; al = the factor, cl = the largest, dx = the image side → ax = the component's blocks that way:
; ceil(ceil(side × factor / largest) / 8)
.blocks:
        push    bx
        xor     ah,ah
        mul     dx
        xor     ch,ch
        add     ax,cx
        adc     dx,0
        sub     ax,1
        sbb     dx,0
        div     cx
        add     ax,7
        shr     ax,1
        shr     ax,1
        shr     ax,1
        pop     bx
        ret

; before each MCU: every jri of them, a restart marker, and the predictions start over
jrestart:
        cmp     [jri],0
        je      .r
        cmp     [jtogo],0
        jne     .count
        mov     ax,[jri]
        mov     [jtogo],ax
        mov     [jcnt],0
        mov     [jpred],0
        mov     [jpred+2],0
        mov     [jpred+4],0
        mov     al,[jmarker]
        test    al,al
        jnz     .have
.find:  call    rd_byte                 ; the marker, not read yet
        cmp     al,0FFh
        jne     .find
@@:     call    rd_byte
        cmp     al,0FFh
        je      @b
        test    al,al                   ; stuffed data, not a marker
        jz      .find
.have:  cmp     al,0D0h
        jb      .other
        cmp     al,0D7h
        ja      .other
        mov     [jmarker],0             ; the restart: on with the data
        jmp     .count
.other: mov     [jmarker],al            ; the scan's end: jat_end sees it
.count: dec     [jtogo]
.r:     ret

; the block jbx, jby of component jcur: its DC, its AC skipped, its mean into acc
jblock:
        mov     bl,[jcur]
        xor     bh,bh
        mov     al,[jc_td+bx]
        xor     ah,ah
        mov     si,J_TABLE
        mul     si
        mov     si,ax
        add     si,jhuff
        call    jdecode
        cmp     al,11
        ja      .bad
        mov     cl,al
        xor     dx,dx
        test    cl,cl
        jz      .dc
        call    jbits                   ; extend: below 2^(s-1) it is negative
        mov     dx,ax
        mov     ax,1
        dec     cl
        shl     ax,cl
        inc     cl
        cmp     dx,ax
        jae     .dc
        mov     ax,-1
        shl     ax,cl
        inc     ax
        add     dx,ax
.dc:    shl     bx,1
        add     dx,[jpred+bx]
        mov     [jpred+bx],dx
        shr     bx,1
        cmp     [jprog],0
        jne     .mean
        ; baseline: the AC coefficients, skipped
        mov     al,[jc_ta+bx]
        xor     ah,ah
        add     ax,4
        mov     si,J_TABLE
        mul     si
        mov     si,ax
        add     si,jhuff
        mov     di,1
.ac:    cmp     di,64
        jae     .mean
        call    jdecode
        mov     cl,al
        and     cl,0Fh
        shr     al,1
        shr     al,1
        shr     al,1
        shr     al,1
        test    cl,cl
        jnz     .coef
        cmp     al,15
        jne     .mean                   ; end of block
        add     di,16
        jmp     .ac
.coef:  xor     ah,ah
        add     di,ax
        inc     di
        call    jbits
        jmp     .ac
.mean:  ; only every jstep-th block each way is averaged
        mov     ax,[jbx]
        xor     dx,dx
        mov     cl,[jstep_x+bx]
        xor     ch,ch
        div     cx
        test    dx,dx
        jnz     .r
        mov     ax,[jby]
        xor     dx,dx
        mov     cl,[jstep_y+bx]
        div     cx
        test    dx,dx
        jnz     .r
        ; the block's origin in the image: past its edge, only padding
        mov     al,[jhmax]
        xor     ah,ah
        mov     cl,[jc_h+bx]
        call    .scale
        mov     cx,[jbx]
        mov     di,[img_w]
        call    .pos
        cmp     ax,[img_w]
        jae     .r
        mov     [jx],dx
        mov     al,[jvmax]
        xor     ah,ah
        mov     cl,[jc_v+bx]
        call    .scale
        mov     cx,[jby]
        mov     di,[img_h]
        call    .pos
        cmp     ax,[img_h]
        jae     .r
        mov     [jy],dx
        ; the mean: DC << Al × the quantiser / 8 + 128, clamped
        push    bx
        shl     bx,1
        mov     ax,[jpred+bx]
        pop     bx
        mov     cl,[jc_tq+bx]
        xor     ch,ch
        mov     si,cx
        shl     si,1
        imul    word [jq+si]            ; dx:ax
        mov     cl,[jal]
        jcxz    .shifted
@@:     shl     ax,1
        rcl     dx,1
        loop    @b
.shifted:
        mov     cx,3
@@:     sar     dx,1
        rcr     ax,1
        loop    @b
        add     ax,128
        adc     dx,0
        test    dx,dx
        js      .black
        jnz     .white
        cmp     ax,255
        jbe     .v
.white: mov     ax,255
        jmp     .v
.black: xor     ax,ax
.v:     push    ax
        ; how much of the image the block stands for: (max / h) × (max / v). The same for every
        ; block of a component, and each channel is averaged over its own weights, so a factor
        ; of 3 rounding it down changes nothing
        mov     al,[jhmax]
        xor     ah,ah
        div     byte [jc_h+bx]
        mov     dl,al
        mov     al,[jvmax]
        xor     ah,ah
        div     byte [jc_v+bx]
        mul     dl
        mov     dx,ax
        pop     ax
        mov     cl,bl                   ; the channel: Y, Cb, Cr in frame order
        call    acc_add_c
.r:     ret
.bad:   mov     ax,s_jpeg_bad
        jmp     dec_fail
; ax = the largest factor, cl = the component's → jsc_num (the image pixels in 8 of its pixels,
; × the factor: 8 × max) and jsc_den (its factor)
.scale: shl     ax,1
        shl     ax,1
        shl     ax,1
        mov     [jsc_num],ax
        xor     ch,ch
        mov     [jsc_den],cx
        ret
; cx = the block, di = the image's side → ax = its origin in the image, dx = its centre,
; clamped to the image
.pos:   mov     ax,cx
        mul     [jsc_num]
        div     [jsc_den]
        push    ax
        mov     ax,cx
        shl     ax,1
        inc     ax                      ; (2 × block + 1) × 8 × max / (2 × h): the centre
        mul     [jsc_num]
        mov     cx,[jsc_den]
        shl     cx,1
        div     cx
        mov     dx,ax
        pop     ax
        dec     di
        cmp     dx,di
        jbe     @f
        mov     dx,di
@@:     ret
