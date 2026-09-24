; JPEG.ASM, included by DISKONAU.ASM: a JPEG decoded into the preview's half blocks
; (PREVIEW.ASM).
;
; A block pixel of the preview covers many 8x8 blocks of the image, and the mean of a block is
; its DC coefficient alone: DC x the quantiser / 8 + 128. So no inverse DCT: the Huffman codes
; are decoded (the AC ones only to be skipped) and each block's mean is averaged into acc, as Y,
; Cb and Cr, converted to RGB at the end. Baseline and extended sequential JPEGs, and
; progressive ones from their first DC scans (the AC scans are skipped). Grayscale or YCbCr, any
; sampling factors, restart intervals. Not lossless, arithmetic-coded, 12-bit or CMYK.

J_TABLE         = 32+256        ; a Huffman table: 16 word counts, then up to 256 symbols

jpeg_decode:
        call    rd_be16                 ; FFD8, known already
        mov     [jsof],0
        mov     [jri],0
        mov     [jdone],0
        mov     [jmarker],0
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
.skip:  call    rd_be16
        movzx   ecx,ax
        sub     ecx,2
        jb      .bad
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

.sofp:  mov     [jprog],1
        jmp     .frame
.sof:   mov     [jprog],0
.frame: call    rd_be16                 ; length
        call    rd_byte
        cmp     al,8
        jne     .kind
        call    rd_be16
        movzx   eax,ax
        mov     [img_h],eax
        call    rd_be16
        movzx   eax,ax
        mov     [img_w],eax
        cmp     dword [img_h],0         ; the height in a DNL marker
        je      .kind
        cmp     dword [img_w],0
        je      .bad
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
        shr     ah,4
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
        jmp     .marker
.kind:  mov     ax,s_jpeg_kind
        jmp     dec_fail
.cmyk:  mov     ax,s_jpeg_cmyk
        jmp     dec_fail

.dht:   call    rd_be16
        movzx   ebp,ax
        sub     ebp,2
.table: cmp     ebp,17
        jl      .marker
        call    rd_byte
        mov     ah,al
        shr     ah,4                    ; 0 DC, 1 AC
        cmp     ah,1
        ja      .bad
        and     al,3
        shl     ah,2
        add     al,ah
        movzx   di,al
        imul    di,di,J_TABLE
        add     di,jhuff
        xor     edx,edx                 ; how many symbols
        mov     cx,16
.cnt:   call    rd_byte
        movzx   ax,al
        mov     [di],ax
        add     dx,ax
        add     di,2
        loop    .cnt
        sub     di,32
        cmp     dx,256
        ja      .bad
        sub     ebp,17
        sub     ebp,edx
        jb      .bad
        mov     cx,dx
        xor     bx,bx
        jcxz    .table
@@:     call    rd_byte
        mov     [di+32+bx],al
        inc     bx
        loop    @b
        jmp     .table

.dqt:   call    rd_be16
        movzx   ebp,ax
        sub     ebp,2
.q:     cmp     ebp,65
        jl      .marker
        call    rd_byte
        mov     bl,al
        and     bx,3
        shl     bx,1
        shr     al,4
        mov     ecx,63
        sub     ebp,65
        test    al,al
        jz      .q8
        call    rd_be16                 ; 16-bit values
        mov     [jq+bx],ax
        mov     ecx,126
        sub     ebp,64
        jb      .bad
        jmp     .qrest
.q8:    call    rd_byte
        movzx   ax,al
        mov     [jq+bx],ax
.qrest: call    rd_skip                 ; only the DC one matters
        jmp     .q

.dri:   call    rd_be16
        call    rd_be16
        mov     [jri],ax
        jmp     .marker

.sos:   cmp     [jsof],0
        je      .bad
        call    rd_be16
        call    rd_byte
        mov     byte [jnscan],al           ; a word, compared with si
        cmp     al,1
        jb      .bad
        cmp     al,[jnf]
        ja      .bad
        xor     si,si
.sc:    cmp     si,word [jnscan]
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
        shr     ah,4
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
        shr     ah,4
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
        call    jscan
        xor     si,si
@@:     cmp     si,word [jnscan]
        jae     @f
        movzx   bx,[jsc+si]
        mov     al,1
        mov     cl,bl
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

; "JPEG 1920x1080"
jpeg_describe:
        push    es
        push    ds
        pop     es
        mov     di,pv_desc
        mov     si,s_jpeg
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

; the next bit, most significant first → eax
jbit:
        cmp     [jcnt],0
        jne     @f
        push    ax
        call    jbyte
        mov     [jbits_buf],al
        pop     ax
        mov     [jcnt],8
@@:     mov     al,[jbits_buf]
        shl     [jbits_buf],1
        dec     [jcnt]
        shr     al,7
        movzx   eax,al
        ret

; cl bits → eax
jbits:
        push    edx
        push    cx
        xor     edx,edx
        test    cl,cl
        jz      .r
@@:     call    jbit
        shl     edx,1
        or      edx,eax
        dec     cl
        jnz     @b
.r:     mov     eax,edx
        pop     cx
        pop     edx
        ret

; si = a table → eax = the next symbol
jdecode:
        push    ebx
        push    ecx
        push    edx
        push    edi
        push    esi
        movzx   esi,si
        xor     edx,edx
        xor     ebx,ebx
        xor     edi,edi
        mov     ecx,1
.l:     call    jbit
        or      edx,eax
        movzx   eax,word [esi+ecx*2-2]  ; codes of length ecx: JPEG counts from 1
        add     ebx,eax
        cmp     edx,ebx
        jb      .found
        add     edi,eax
        shl     ebx,1
        shl     edx,1
        inc     ecx
        cmp     ecx,16
        jbe     .l
        mov     ax,s_jpeg_bad
        jmp     dec_fail
.found: sub     edx,ebx
        add     edx,eax
        add     edx,edi
        movzx   eax,byte [esi+32+edx]
        pop     esi
        pop     edi
        pop     edx
        pop     ecx
        pop     ebx
        ret

; one scan's blocks
jscan:
        mov     [jcnt],0
        xor     eax,eax
        mov     [jpred],eax
        mov     [jpred+4],eax
        mov     [jpred+8],eax
        mov     ax,[jri]
        mov     [jtogo],ax
        cmp     [jnscan],1
        jne     .mcus
        ; one component: its own blocks, row by row
        movzx   bx,[jsc]
        mov     [jcur],bl
        movzx   eax,[jc_h+bx]
        mul     dword [img_w]
        movzx   ecx,[jhmax]
        add     eax,ecx
        dec     eax
        xor     edx,edx
        div     ecx
        add     eax,7
        shr     eax,3
        mov     [jbw],eax
        movzx   eax,[jc_v+bx]
        mul     dword [img_h]
        movzx   ecx,[jvmax]
        add     eax,ecx
        dec     eax
        xor     edx,edx
        div     ecx
        add     eax,7
        shr     eax,3
        mov     [jbh],eax
        mov     dword [jby],0
.r1:    mov     eax,[jby]
        cmp     eax,[jbh]
        jae     .done
        mov     dword [jbx],0
.c1:    mov     eax,[jbx]
        cmp     eax,[jbw]
        jae     .n1
        call    jrestart
        call    jblock
        inc     dword [jbx]
        jmp     .c1
.n1:    inc     dword [jby]
        jmp     .r1
.mcus:  ; interleaved: MCU by MCU
        movzx   ecx,[jhmax]
        shl     ecx,3
        mov     eax,[img_w]
        add     eax,ecx
        dec     eax
        xor     edx,edx
        div     ecx
        mov     [jmcux],eax
        movzx   ecx,[jvmax]
        shl     ecx,3
        mov     eax,[img_h]
        add     eax,ecx
        dec     eax
        xor     edx,edx
        div     ecx
        mov     [jmcuy],eax
        mov     dword [jmy],0
.rm:    mov     eax,[jmy]
        cmp     eax,[jmcuy]
        jae     .done
        mov     dword [jmx],0
.cm:    mov     eax,[jmx]
        cmp     eax,[jmcux]
        jae     .nm
        call    jrestart
        xor     si,si
.k:     cmp     si,word [jnscan]
        jae     .kd
        movzx   bx,[jsc+si]
        mov     [jcur],bl
        mov     byte [jv],0
.v:     mov     al,[jv]
        cmp     al,[jc_v+bx]
        jae     .kn
        mov     byte [jh],0
.h:     mov     al,[jh]
        cmp     al,[jc_h+bx]
        jae     .vn
        movzx   eax,[jc_h+bx]           ; the block: mx*Hc + h, my*Vc + v
        mul     dword [jmx]
        movzx   ecx,[jh]
        add     eax,ecx
        mov     [jbx],eax
        movzx   eax,[jc_v+bx]
        mul     dword [jmy]
        movzx   ecx,[jv]
        add     eax,ecx
        mov     [jby],eax
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
.kd:    inc     dword [jmx]
        jmp     .cm
.nm:    inc     dword [jmy]
        test    byte [jmy],7            ; now and then, whether a key is waiting
        jnz     .rm
        call    key_ready
        jz      .rm
        xor     ax,ax
        jmp     dec_fail
.done:  ret

; before each MCU: every jri of them, a restart marker, and the predictions start over
jrestart:
        cmp     [jri],0
        je      .r
        cmp     [jtogo],0
        jne     .count
        mov     ax,[jri]
        mov     [jtogo],ax
        mov     [jcnt],0
        xor     eax,eax
        mov     [jpred],eax
        mov     [jpred+4],eax
        mov     [jpred+8],eax
        mov     al,[jmarker]
        test    al,al
        jnz     .have
@@:     call    rd_byte                 ; the marker, not read yet
        cmp     al,0FFh
        jne     @b
@@:     call    rd_byte
        cmp     al,0FFh
        je      @b
.have:  cmp     al,0D0h
        jb      .count
        cmp     al,0D7h
        ja      .count
        mov     [jmarker],0
.count: dec     [jtogo]
.r:     ret

; the block jbx, jby of component jcur: its DC, its AC skipped, its mean into acc
jblock:
        movzx   bx,[jcur]
        movzx   si,[jc_td+bx]
        imul    si,si,J_TABLE
        add     si,jhuff
        call    jdecode
        cmp     al,11
        ja      .bad
        mov     cl,al
        xor     edx,edx
        test    cl,cl
        jz      .dc
        call    jbits                   ; extend: below 2^(s-1) it is negative
        mov     edx,eax
        mov     eax,1
        dec     cl
        shl     eax,cl
        inc     cl
        cmp     edx,eax
        jae     .dc
        mov     eax,-1
        shl     eax,cl
        inc     eax
        add     edx,eax
.dc:    shl     bx,2
        add     edx,[jpred+bx]
        mov     [jpred+bx],edx
        shr     bx,2
        cmp     [jprog],0
        jne     .mean
        ; baseline: the AC coefficients, skipped
        movzx   si,[jc_ta+bx]
        add     si,4
        imul    si,si,J_TABLE
        add     si,jhuff
        mov     di,1
.ac:    cmp     di,64
        jae     .mean
        call    jdecode
        mov     cl,al
        and     cl,0Fh
        shr     al,4
        test    cl,cl
        jnz     .coef
        cmp     al,15
        jne     .mean                   ; end of block
        add     di,16
        jmp     .ac
.coef:  movzx   ax,al
        add     di,ax
        inc     di
        call    jbits
        jmp     .ac
.mean:  shl     bx,2
        mov     eax,[jpred+bx]
        shr     bx,2
        mov     cl,[jal]
        shl     eax,cl
        movzx   si,[jc_tq+bx]
        shl     si,1
        movzx   ecx,word [jq+si]
        imul    eax,ecx
        sar     eax,3
        add     eax,128
        jns     @f
        xor     eax,eax
@@:     cmp     eax,255
        jbe     @f
        mov     eax,255
@@:     push    eax
        ; the block's centre in the image, and how much of it the block stands for
        movzx   ecx,[jhmax]
        mov     eax,[jbx]
        shl     eax,3
        add     eax,4
        mul     ecx
        movzx   ecx,[jc_h+bx]
        div     ecx
        mov     [jx],eax
        movzx   ecx,[jvmax]
        mov     eax,[jby]
        shl     eax,3
        add     eax,4
        mul     ecx
        movzx   ecx,[jc_v+bx]
        div     ecx
        mov     [jy],eax
        pop     eax
        mov     ecx,[jx]
        cmp     ecx,[img_w]
        jae     .r
        mov     ecx,[jy]
        cmp     ecx,[img_h]
        jae     .r
        push    eax
        movzx   eax,[jhmax]
        xor     edx,edx
        movzx   ecx,[jc_h+bx]
        div     ecx
        push    eax
        movzx   eax,[jvmax]
        xor     edx,edx
        movzx   ecx,[jc_v+bx]
        div     ecx
        pop     edx
        imul    edx,eax                 ; the weight
        pop     eax
        mov     cl,bl                   ; the channel: Y, Cb, Cr in frame order
        call    acc_add_c
.r:     ret
.bad:   mov     ax,s_jpeg_bad
        jmp     dec_fail
