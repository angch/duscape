; PREVIEW.ASM, included by DISKONAU.ASM: what the preview under the list shows for the file in
; hand. The reference is common/src/preview.rs (sniff, text_lines) and viewers/tui/src/preview.rs
; (block_image): the first lines of a text file; a PNG or JPEG drawn in half blocks, two pixels to
; a cell; else a line saying what the file is.
;
; Pictures are averaged into at most PV_COLS x PV_HROWS pixels (acc), fitted like the image
; crate's `thumbnail`, never scaled up. Text mode has 16 colours; ten are the interface's, and
; the other six (ADAPTIVE) are set in the VGA DAC to the colours the picture needs most, picked
; farthest-first and refined once, for as long as the picture is on screen.
;
; Decoding (PNG.ASM, JPEG.ASM) reads the file through rd_byte and gives up through dec_fail,
; which unwinds to pv_decode from any depth. A key pressed meanwhile stops it; it starts again
; when the keyboard has been quiet for a moment, as the terminal viewer waits 100 ms.

PV_NONE         = 0
PV_TEXT         = 1
PV_INFO         = 2
PV_PICTURE      = 3
PV_PENDING      = 4

K_EMPTY         = 0
K_PNG           = 1
K_JPEG          = 2
K_TEXT          = 3
K_BINARY        = 4

; ax = the node to preview, or 0. Reads the head of a new one and decides what it is; a picture
; is left PV_PENDING for pv_decode.
pv_update:
        cmp     ax,[pv_node]
        je      .r
        mov     [pv_node],ax
        mov     [pv_state],PV_NONE
        test    ax,ax
        jz      .r
        push    es
        push    ds
        pop     es
        mov     di,pvpath
        call    node_path
        call    pv_head
        jc      .err
        call    sniff
        mov     [pv_kind],al
        cmp     al,K_TEXT
        je      .text
        cmp     al,K_BINARY
        je      .binary
        cmp     al,K_EMPTY
        je      .empty
        mov     [pv_state],PV_PENDING
        jmp     .out
.text:  call    text_lines
        mov     [pv_state],PV_TEXT
        jmp     .out
.binary:
        mov     si,s_binary
        jmp     .info
.empty: mov     si,s_emptyfile
.info:  call    pv_set_info
        jmp     .out
.err:   mov     si,s_unreadable
        call    pv_info_doserr
.out:   pop     es
.r:     ret

; the info line: si, zero-terminated
pv_set_info:
        mov     di,pv_info
        call    append_z
        mov     byte [di],0
        mov     [pv_state],PV_INFO
        ret

; the info line: si, then "(DOS error ax)"
pv_info_doserr:
        push    ax
        mov     di,pv_info
        call    append_z
        mov     si,s_doserr_paren
        call    append_z
        pop     ax
        movzx   eax,ax
        call    u32dec
        mov     al,')'
        stosb
        mov     byte [di],0
        mov     [pv_state],PV_INFO
        ret

; pvpath opened for reading: bx = handle, CF=1 and ax = error on failure
pv_open:
        cmp     [lfn],0
        je      .s
        mov     ax,716Ch
        xor     bx,bx                   ; read only
        xor     cx,cx
        mov     dx,1                    ; open if it exists
        mov     si,pvpath
        stc
        int     21h
        jc      .s
        mov     bx,ax
        ret
.s:     mov     ax,3D00h
        mov     dx,pvpath
        int     21h
        mov     bx,ax
        ret

; the first HEAD_BYTES of pvpath into lineseg:0, the count in pv_headlen. CF=1, ax = error.
pv_head:
        call    pv_open
        jc      .r
        push    ds
        mov     ax,[lineseg]
        xor     di,di
        mov     dword [pv_headlen],0
        mov     ds,ax
        xor     dx,dx
        mov     cx,8000h
        mov     ah,3Fh
        int     21h
        jc      .fail
        mov     di,ax
        cmp     ax,8000h
        jb      .done
        mov     dx,8000h
        mov     cx,8000h
        mov     ah,3Fh
        int     21h
        jc      .fail
        add     di,ax
        jnc     .done
        pop     ds                      ; all of it: 65536
        mov     dword [pv_headlen],10000h
        jmp     .close
.done:  pop     ds
        movzx   eax,di
        mov     [pv_headlen],eax
.close: mov     ah,3Eh
        int     21h
        clc
.r:     ret
.fail:  pop     ds
        push    ax
        mov     ah,3Eh
        int     21h
        pop     ax
        stc
        ret

; what the head is (preview::sniff) → al
sniff:
        push    fs
        mov     fs,[lineseg]
        mov     ecx,[pv_headlen]
        mov     al,K_EMPTY
        jecxz   .out
        cmp     ecx,8
        jb      .jpeg
        mov     eax,[fs:0]
        cmp     eax,474E5089h
        jne     .jpeg
        mov     eax,[fs:4]
        cmp     eax,0A1A0A0Dh
        jne     .jpeg
        mov     al,K_PNG
        jmp     .out
.jpeg:  cmp     ecx,3
        jb      .scan
        cmp     word [fs:0],0D8FFh
        jne     .scan
        cmp     byte [fs:2],0FFh
        jne     .scan
        mov     al,K_JPEG
        jmp     .out
.scan:  xor     esi,esi
        xor     edx,edx                 ; control bytes
.l:     mov     al,[fs:si]
        test    al,al
        jz      .binary
        cmp     al,20h
        jae     .n
        cmp     al,9
        je      .n
        cmp     al,0Ah
        je      .n
        cmp     al,0Dh
        je      .n
        cmp     al,0Ch
        je      .n
        cmp     al,1Bh
        je      .n
        inc     edx
.n:     inc     si
        inc     esi
        cmp     esi,ecx
        jb      .l
        imul    edx,edx,100
        cmp     edx,ecx
        ja      .binary
        mov     al,K_TEXT
        jmp     .out
.binary:
        mov     al,K_BINARY
.out:   pop     fs
        ret

; the head's first PV_ROWS lines, as preview::text_lines makes them and cut to PV_COLS: tabs to
; the next multiple of four, control characters as '?', UTF-8 in CP437 where it has the
; character ('?' where not), a sequence cut off at the end of the head dropped
text_lines:
        push    fs
        push    es
        push    ds
        pop     es
        mov     fs,[lineseg]
        xor     esi,esi
        mov     [pv_nlines],0
.line:  cmp     [pv_nlines],PV_ROWS
        jae     .done
        cmp     esi,[pv_headlen]
        jae     .done
        movzx   di,[pv_nlines]
        shl     di,5
        add     di,pv_text
        xor     dx,dx                   ; the column
.ch:    cmp     esi,[pv_headlen]
        jae     .eol
        call    utf8_next               ; ax = the character
        jc      .eol
        cmp     ax,0Ah
        je      .eol
        cmp     ax,0Dh
        jne     .tab
        cmp     esi,[pv_headlen]        ; "\r\n" ends the line; a lone \r is a control
        jae     .ctl
        cmp     byte [fs:si],0Ah
        jne     .ctl
        inc     esi
        jmp     .eol
.tab:   cmp     ax,9
        jne     .other
        mov     al,' '
@@:     call    .put
        test    dx,3
        jnz     @b
        jmp     .ch
.other: cmp     ax,20h
        jb      .ctl
        cmp     ax,7Fh
        jb      .ascii
        cmp     ax,0A0h                 ; DEL and the C1 controls
        jb      .ctl
        push    di
        mov     di,cp437_hi
        mov     cx,128
@@:     cmp     [di],ax
        je      @f
        add     di,2
        loop    @b
        pop     di
        mov     al,'?'
        jmp     .one
@@:     sub     di,cp437_hi
        shr     di,1
        mov     ax,di
        add     al,80h
        pop     di
        jmp     .one
.ctl:   mov     al,'?'
        jmp     .one
.ascii:
.one:   call    .put
        jmp     .ch
.eol:   movzx   bx,[pv_nlines]
        cmp     dx,PV_COLS
        jbe     @f
        mov     dx,PV_COLS
@@:     mov     [pv_tlen+bx],dl
        inc     [pv_nlines]
        jmp     .line
.done:  pop     es
        pop     fs
        ret
; al at column dx, if it is within the preview
.put:   cmp     dx,PV_COLS
        jae     @f
        push    di
        add     di,dx
        mov     [di],al
        pop     di
@@:     inc     dx
        ret

; the character at fs:si (esi counts too) → ax, both advanced; U+FFFD as '?'. CF=1 when the
; head ends inside a sequence.
utf8_next:
        mov     al,[fs:si]
        inc     esi
        xor     ah,ah
        cmp     al,80h
        jb      .r
        mov     cl,1
        mov     bx,1Fh
        cmp     al,0C2h
        jb      .bad
        cmp     al,0E0h
        jb      .seq
        mov     cl,2
        mov     bx,0Fh
        cmp     al,0F0h
        jb      .seq
        mov     cl,3
        mov     bx,07h
        cmp     al,0F5h
        jae     .bad
.seq:   and     bx,ax                   ; the lead byte's bits
.more:  cmp     esi,[pv_headlen]
        jae     .cut
        mov     al,[fs:si]
        and     al,0C0h
        cmp     al,80h
        jne     .bad                    ; not a continuation: this one alone is replaced
        mov     al,[fs:si]
        and     ax,3Fh
        shl     bx,6
        or      bx,ax
        inc     esi
        dec     cl
        jnz     .more
        mov     ax,bx                   ; beyond U+FFFF there is nothing in CP437 anyway
        clc
        ret
.bad:   mov     ax,'?'
.r:     clc
        ret
.cut:   stc
        ret

; ---------------------------------------------------------------------------------------------
; reading a picture

; al → the next byte of the file; the end of it gives up
rd_byte:
        push    bx
        mov     bx,[io_pos]
        cmp     bx,[io_len]
        jb      .have
        call    io_fill
        xor     bx,bx
.have:  mov     al,[iobuf+bx]
        inc     bx
        mov     [io_pos],bx
        pop     bx
        ret

io_fill:
        push    ax
        push    cx
        push    dx
        push    si
        call    key_ready               ; typing goes first; the picture can wait
        jnz     .abort
        mov     ah,3Fh
        mov     bx,[pv_handle]
        mov     cx,IOBUF
        mov     dx,iobuf
        int     21h
        jc      .short
        test    ax,ax
        jz      .short
        mov     [io_len],ax
        mov     [io_pos],0
        pop     si
        pop     dx
        pop     cx
        pop     ax
        ret
.short: mov     ax,s_truncated
        jmp     dec_fail
.abort: xor     ax,ax
        jmp     dec_fail

; eax ← the next four bytes, big-endian
rd_be32:
        push    cx
        mov     cx,4
@@:     shl     eax,8
        call    rd_byte
        loop    @b
        pop     cx
        ret

; ax ← the next two bytes, big-endian
rd_be16:
        call    rd_byte
        mov     ah,al
        call    rd_byte
        ret

; skip ecx bytes
rd_skip:
        jecxz   .r
@@:     call    rd_byte
        dec     ecx
        jnz     @b
.r:     ret

; give up decoding: ax = why (a zero-terminated string), or 0 when a key interrupted
dec_fail:
        mov     [dec_err],ax
        mov     sp,[dec_sp]
        fninit
        fldcw   [cw_near]
        stc
        ret

; decode the pending picture (PNG.ASM or JPEG.ASM) into blocks, or say why not
pv_decode:
        push    es
        push    fs
        push    gs
        pushad
        mov     di,pvpath
        push    ds
        pop     es
        call    pv_open
        jc      .openerr
        mov     [pv_handle],bx
        mov     [io_pos],0
        mov     [io_len],0
        call    .run
        pushf
        mov     bx,[pv_handle]
        mov     ah,3Eh
        int     21h
        popf
        jc      .failed
        call    acc_finish
        mov     [pv_state],PV_PICTURE
        mov     [pal_want],1
        jmp     .out
.failed:
        cmp     [dec_err],0
        je      .out                    ; interrupted: still pending
        mov     di,pv_info
        mov     si,s_png_unreadable
        cmp     [pv_kind],K_PNG
        je      @f
        mov     si,s_jpeg_unreadable
@@:     push    ds
        pop     es
        call    append_z
        mov     si,[dec_err]
        call    append_z
        mov     byte [di],0
        mov     [pv_state],PV_INFO
        jmp     .out
.openerr:
        mov     si,s_unreadable
        call    pv_info_doserr
.out:   popad
        pop     gs
        pop     fs
        pop     es
        ret
.run:   mov     [dec_sp],sp
        push    ds
        pop     es
        cmp     [pv_kind],K_PNG
        je      png_decode
        jmp     jpeg_decode

; ---------------------------------------------------------------------------------------------
; the half-block picture

; img_w x img_h → acc_cols x acc_hrows, and the sums cleared. Fitted into PV_COLS*8 by
; PV_ROWS*16 pixels (cells are 8x16), never scaled up; a pixel is half a cell.
acc_setup:
        push    es
        push    ds
        pop     es
        mov     eax,[img_w]
        mov     [fit_w],eax
        mov     eax,[img_h]
        mov     [fit_h],eax
        cmp     dword [img_w],PV_COLS*8
        ja      .scale
        cmp     dword [img_h],PV_ROWS*16
        jbe     .sized
.scale: fild    dword [c_pw]
        fidiv   dword [img_w]
        fild    dword [c_ph]
        fidiv   dword [img_h]
        fcom    st1
        fnstsw  ax
        sahf
        jb      @f
        fxch
@@:     fstp    st1                     ; the smaller ratio
        fild    dword [img_w]
        fmul    st0,st1
        call    round_u32
        mov     [fit_w],eax
        fild    dword [img_h]
        fmul    st0,st1
        call    round_u32
        mov     [fit_h],eax
        fstp    st0
        cmp     dword [fit_w],0
        jne     @f
        mov     dword [fit_w],1
@@:     cmp     dword [fit_h],0
        jne     .sized
        mov     dword [fit_h],1
.sized: mov     eax,[fit_w]
        add     eax,7
        shr     eax,3
        cmp     eax,PV_COLS
        jbe     @f
        mov     eax,PV_COLS
@@:     mov     [acc_cols],eax
        mov     eax,[fit_h]
        add     eax,7
        shr     eax,3
        cmp     eax,PV_HROWS
        jbe     @f
        mov     eax,PV_HROWS
@@:     mov     [acc_hrows],eax
        ; sampling: about eight source pixels a side for each block pixel is plenty
        mov     eax,[img_w]
        xor     edx,edx
        mov     ecx,[acc_cols]
        shl     ecx,3
        div     ecx
        test    eax,eax
        jnz     @f
        inc     eax
@@:     mov     [step_x],eax
        mov     eax,[img_h]
        xor     edx,edx
        mov     ecx,[acc_hrows]
        shl     ecx,3
        div     ecx
        test    eax,eax
        jnz     @f
        inc     eax
@@:     mov     [step_y],eax
        mov     di,acc
        xor     eax,eax
        mov     cx,PV_COLS*PV_HROWS*ACC_SIZE/4
        rep     stosd
        pop     es
        ret

; eax = y: the block row it falls in, for acc_add
acc_row:
        push    edx
        mul     dword [acc_hrows]
        div     dword [img_h]
        mul     dword [acc_cols]
        shl     ax,5
        mov     [acc_base],ax
        pop     edx
        ret

; eax = x, px_r px_g px_b px_a: one pixel of the row acc_row chose
acc_add:
        push    bx
        push    edx
        mul     dword [acc_cols]
        div     dword [img_w]
        shl     ax,5
        add     ax,[acc_base]
        mov     bx,ax
        movzx   eax,[px_r]
        add     dword [acc+bx],eax
        movzx   eax,[px_g]
        add     dword [acc+bx+4],eax
        movzx   eax,[px_b]
        add     dword [acc+bx+8],eax
        movzx   eax,[px_a]
        add     dword [acc+bx+12],eax
        inc     dword [acc+bx+16]
        pop     edx
        pop     bx
        ret

; jx, jy, cl = channel (0 Y, 1 Cb, 2 Cr), eax = value, edx = weight: one JPEG block's mean
acc_add_c:
        push    bx
        push    si
        push    eax
        push    edx
        mov     eax,[jy]
        mul     dword [acc_hrows]
        div     dword [img_h]
        mul     dword [acc_cols]
        mov     si,ax
        mov     eax,[jx]
        mul     dword [acc_cols]
        div     dword [img_w]
        add     ax,si
        shl     ax,5
        mov     bx,ax
        movzx   si,cl
        shl     si,2
        add     bx,si
        pop     edx
        pop     eax
        push    eax
        push    edx
        imul    eax,edx
        add     dword [acc+bx],eax
        add     dword [acc+bx+16],edx
        pop     edx
        pop     eax
        pop     si
        pop     bx
        ret

; the sums → tcol (r, g, b, opaque), then the palette and the colour of each block pixel (blk)
acc_finish:
        push    es
        push    ds
        pop     es
        mov     eax,[acc_cols]
        mul     dword [acc_hrows]
        mov     [acc_n],ax
        xor     si,si                   ; acc
        xor     di,di                   ; tcol
        mov     cx,[acc_n]
.px:    mov     byte [tcol+di+3],0
        cmp     [acc_mode],0
        jne     .ycc
        mov     ebx,dword [acc+si+16]
        test    ebx,ebx
        jz      .next
        mov     eax,dword [acc+si+12]
        xor     edx,edx
        div     ebx
        cmp     eax,128
        jb      .next
        mov     eax,dword [acc+si]
        xor     edx,edx
        div     ebx
        mov     [tcol+di],al
        mov     eax,dword [acc+si+4]
        xor     edx,edx
        div     ebx
        mov     [tcol+di+1],al
        mov     eax,dword [acc+si+8]
        xor     edx,edx
        div     ebx
        mov     [tcol+di+2],al
        mov     byte [tcol+di+3],1
        jmp     .next
.ycc:   cmp     dword [acc+si+16],0
        je      .next
        push    cx
        xor     bx,bx
.ch:    mov     dword [ycc+bx],128      ; a channel with no block here is neutral
        mov     ecx,dword [acc+si+bx+16]
        jecxz   @f
        fild    dword [acc+si+bx]
        fidiv   dword [acc+si+bx+16]
        fistp   dword [ycc+bx]
@@:     add     bx,4
        cmp     bx,12
        jb      .ch
        pop     cx
        fild    dword [ycc+8]           ; R = Y + 1.402 (Cr - 128)
        fisub   dword [c_128]
        fmul    qword [c_crr]
        fiadd   dword [ycc]
        call    .clamp
        mov     [tcol+di],al
        fild    dword [ycc+4]           ; G = Y - 0.344136 (Cb - 128) - 0.714136 (Cr - 128)
        fisub   dword [c_128]
        fmul    qword [c_cbg]
        fild    dword [ycc+8]
        fisub   dword [c_128]
        fmul    qword [c_crg]
        faddp   st1,st0
        fisubr  dword [ycc]
        call    .clamp
        mov     [tcol+di+1],al
        fild    dword [ycc+4]           ; B = Y + 1.772 (Cb - 128)
        fisub   dword [c_128]
        fmul    qword [c_cbb]
        fiadd   dword [ycc]
        call    .clamp
        mov     [tcol+di+2],al
        mov     byte [tcol+di+3],1
.next:  add     si,ACC_SIZE
        add     di,4
        dec     cx
        jnz     .px
        call    pal_choose
        pop     es
        ret
.clamp: fistp   dword [tmp32]
        mov     eax,[tmp32]
        test    eax,eax
        jns     @f
        xor     eax,eax
@@:     cmp     eax,255
        jbe     @f
        mov     eax,255
@@:     ret

; ---------------------------------------------------------------------------------------------
; colours

; the attribute colours' RGB, from the DAC they point at, and the adaptive ones' to restore
pal_init:
        push    es
        push    ds
        pop     es
        xor     bx,bx
.l:     push    bx
        mov     ax,1007h
        int     10h
        mov     al,bh
        pop     bx
        mov     [dacidx+bx],al
        push    bx
        movzx   bx,al
        mov     ax,1015h
        int     10h
        pop     bx
        push    bx
        imul    bx,bx,3
        mov     al,dh
        call    .eight
        mov     [pal_rgb+bx],al
        mov     al,ch
        call    .eight
        mov     [pal_rgb+bx+1],al
        mov     al,cl
        call    .eight
        mov     [pal_rgb+bx+2],al
        pop     bx
        inc     bx
        cmp     bx,16
        jb      .l
        mov     si,pal_rgb
        mov     di,pal_orig
        mov     cx,48
        rep     movsb
        pop     es
        ret
.eight: and     al,3Fh                  ; six bits → eight
        mov     ah,al
        shl     al,2
        shr     ah,4
        or      al,ah
        ret

; the six adaptive colours for tcol, then each block pixel's nearest colour → blk
pal_choose:
        mov     si,pal_orig             ; start from the interface's colours
        mov     di,pal_rgb
        mov     cx,48
        rep     movsb
        ; farthest first: each slot takes the pixel worst served so far
        xor     bp,bp
.slot:  cmp     bp,ADAPT_N
        jae     .refine
        mov     [far_d],-1
        mov     [far_i],-1
        xor     di,di
.f:     cmp     di,[acc_n]
        jae     .fd
        mov     bx,di
        shl     bx,2
        cmp     byte [tcol+bx+3],0
        je      .fn
        call    nearest_in              ; bx = pixel → eax = distance, dl = colour; only the
                                        ; slots chosen so far count
        cmp     [far_i],-1
        je      .take
        cmp     eax,[far_d]
        jbe     .fn
.take:  mov     [far_d],eax
        mov     [far_i],di
.fn:    inc     di
        jmp     .f
.fd:    cmp     [far_i],-1
        je      .refine
        cmp     dword [far_d],0
        je      .refine                 ; every pixel has its colour already
        mov     bx,[far_i]
        shl     bx,2
        mov     si,bp
        movzx   si,byte [adaptive+si]
        imul    si,si,3
        mov     al,[tcol+bx]
        mov     [pal_rgb+si],al
        mov     al,[tcol+bx+1]
        mov     [pal_rgb+si+1],al
        mov     al,[tcol+bx+2]
        mov     [pal_rgb+si+2],al
        inc     bp
        jmp     .slot
.refine:
        mov     [adapt_used],bp
        ; once round: each adaptive colour moves to the mean of the pixels it serves
        mov     di,rsum
        xor     eax,eax
        mov     cx,ADAPT_N*4
        rep     stosd
        xor     di,di
.a:     cmp     di,[acc_n]
        jae     .means
        mov     bx,di
        shl     bx,2
        cmp     byte [tcol+bx+3],0
        je      .an
        call    nearest
        xor     si,si
@@:     cmp     si,[adapt_used]
        jae     .an
        cmp     dl,[adaptive+si]
        je      @f
        inc     si
        jmp     @b
@@:     shl     si,4
        movzx   eax,byte [tcol+bx]
        add     [rsum+si],eax
        movzx   eax,byte [tcol+bx+1]
        add     [rsum+si+4],eax
        movzx   eax,byte [tcol+bx+2]
        add     [rsum+si+8],eax
        inc     dword [rsum+si+12]
.an:    inc     di
        jmp     .a
.means: xor     si,si
.m:     cmp     si,[adapt_used]
        jae     .assign
        mov     bx,si
        shl     bx,4
        mov     ecx,[rsum+bx+12]
        jecxz   .mn
        movzx   di,[adaptive+si]
        imul    di,di,3
        mov     eax,[rsum+bx]
        xor     edx,edx
        div     ecx
        mov     [pal_rgb+di],al
        mov     eax,[rsum+bx+4]
        xor     edx,edx
        div     ecx
        mov     [pal_rgb+di+1],al
        mov     eax,[rsum+bx+8]
        xor     edx,edx
        div     ecx
        mov     [pal_rgb+di+2],al
.mn:    inc     si
        jmp     .m
.assign:
        xor     di,di
.b:     cmp     di,[acc_n]
        jae     .r
        mov     bx,di
        shl     bx,2
        mov     byte [blk+di],0FFh
        cmp     byte [tcol+bx+3],0
        je      .bn
        call    nearest
        mov     [blk+di],dl
.bn:    inc     di
        jmp     .b
.r:     mov     si,pal_rgb
        mov     di,pal_pic
        mov     cx,48
        rep     movsb
        ret

; tcol+bx → dl = the nearest of all 16 colours, eax = how far
nearest:
        push    bp
        mov     bp,ADAPT_N
        call    nearest_in
        pop     bp
        ret

; the same among the interface's colours and the first bp adaptive ones
nearest_in:
        push    cx
        push    si
        push    ebx
        mov     dword [near_d],-1
        xor     cx,cx
.c:     push    cx                      ; an adaptive slot not yet chosen does not count
        xor     si,si
@@:     cmp     si,ADAPT_N
        jae     .ok
        cmp     cl,[adaptive+si]
        je      @f
        inc     si
        jmp     @b
@@:     cmp     si,bp
        jae     .skip
.ok:    pop     cx
        mov     si,cx
        imul    si,si,3
        movzx   eax,byte [tcol+bx]
        movzx   edx,byte [pal_rgb+si]
        sub     eax,edx
        imul    eax,eax
        shl     eax,1
        push    eax
        movzx   eax,byte [tcol+bx+1]
        movzx   edx,byte [pal_rgb+si+1]
        sub     eax,edx
        imul    eax,eax
        shl     eax,2
        pop     edx
        add     edx,eax
        push    edx
        movzx   eax,byte [tcol+bx+2]
        movzx   edx,byte [pal_rgb+si+2]
        sub     eax,edx
        imul    eax,eax
        imul    eax,eax,3
        pop     edx
        add     eax,edx
        cmp     eax,[near_d]
        jae     .n
        mov     [near_d],eax
        mov     [near_c],cl
        jmp     .n
.skip:  pop     cx
.n:     inc     cx
        cmp     cx,16
        jb      .c
        mov     eax,[near_d]
        mov     dl,[near_c]
        pop     ebx
        pop     si
        pop     cx
        ret

; the DAC as the picture wants it (al = 1), or as the interface has it (al = 0)
pal_set:
        cmp     al,[pal_applied]
        je      .r
        mov     [pal_applied],al
        mov     si,pal_orig
        test    al,al
        jz      @f
        mov     si,pal_pic
@@:     xor     di,di
.l:     movzx   bx,[adaptive+di]
        push    bx
        imul    bx,bx,3
        mov     dh,[si+bx]
        mov     ch,[si+bx+1]
        mov     cl,[si+bx+2]
        shr     dh,2
        shr     ch,2
        shr     cl,2
        pop     bx
        movzx   bx,[dacidx+bx]
        mov     ax,1010h
        int     10h
        inc     di
        cmp     di,ADAPT_N
        jb      .l
.r:     ret

; the picture in the preview's body: dl = x, dh = y of its top left, PV_COLS x PV_ROWS cells,
; centred across (draw_blocks)
draw_blocks:
        mov     ax,PV_COLS
        sub     ax,word [acc_cols]
        shr     ax,1
        add     dl,al
        xor     bx,bx                   ; block row pair
.row:   mov     ax,bx
        shl     ax,1
        cmp     ax,word [acc_hrows]
        jae     .r
        xor     cx,cx
.col:   cmp     cx,word [acc_cols]
        jae     .nrow
        push    bx
        push    dx
        mov     ax,bx                   ; upper: row 2*bx
        shl     ax,1
        mul     word [acc_cols]
        add     ax,cx
        mov     si,ax
        mov     al,[blk+si]             ; upper
        mov     ah,0FFh                 ; lower, if there is a row for it
        push    ax
        mov     ax,bx
        shl     ax,1
        inc     ax
        cmp     ax,word [acc_hrows]
        pop     ax
        jae     @f
        add     si,word [acc_cols]
        mov     ah,[blk+si]
@@:     pop     dx
        push    dx
        add     dl,cl
        add     dh,bl
        cmp     al,0FFh
        je      .noup
        cmp     ah,0FFh
        je      .uponly
        shl     ah,4                    ; ▀ in the upper colour over the lower one
        or      ah,al
        mov     al,0DFh
        call    put_ch
        jmp     .cn
.uponly:
        mov     ah,al
        mov     al,0DFh
        call    put_ch
        jmp     .cn
.noup:  cmp     ah,0FFh
        je      .cn
        mov     al,0DCh                 ; ▄ in the lower colour
        call    put_ch
.cn:    pop     dx
        pop     bx
        inc     cx
        jmp     .col
.nrow:  inc     bx
        jmp     .row
.r:     ret
