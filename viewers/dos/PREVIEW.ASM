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
;
; 16-bit throughout: the head is up to 65536 bytes, so its length is two words (pv_len), and the
; sums are 32 bits in pairs of words.

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

PV_MAXSIDE      = 40000         ; the image crate's limit, as decode_picture sets it

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
        mov     [np_disk],1             ; the path as DOS takes it
        call    node_path
        jc      .long
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
        jmp     .info
.long:  mov     si,s_pathlong
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
        xor     dx,dx
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

; the first 65536 bytes of pvpath into lineseg:0, how many in pv_len (two words). CF=1, ax = the
; error.
pv_head:
        call    pv_open
        jc      .r
        mov     word [pv_len],0
        mov     word [pv_len+2],0
        push    ds
        mov     ds,[lineseg]
        xor     dx,dx                   ; two reads of 32 KiB
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
        pop     ds                      ; all of it
        mov     word [pv_len+2],1
        jmp     .close
.done:  pop     ds
        mov     [pv_len],di
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

; ZF=1 when si has reached the end of the head (pv_len; si wraps to 0 after a full 64 KiB)
head_end:
        cmp     word [pv_len+2],0
        jne     .full
        cmp     si,[pv_len]
        ret
.full:  cmp     [hd_wrapped],0          ; 65536 bytes: the end is si = 0 again
        je      .no
        test    si,si
        ret
.no:    or      sp,sp                   ; ZF=0
        ret

; al ← the head's byte at si, and si on; hd_wrapped set when si goes round
head_byte:
        push    es
        mov     es,[lineseg]
        mov     al,[es:si]
        pop     es
        inc     si
        jnz     @f
        mov     [hd_wrapped],1
@@:     ret

; what the head is (preview::sniff) → al
sniff:
        push    es
        mov     es,[lineseg]
        mov     ax,[pv_len]
        or      ax,[pv_len+2]
        mov     al,K_EMPTY
        jz      .out
        cmp     word [pv_len+2],0
        jne     @f
        cmp     word [pv_len],8
        jb      .jpeg
@@:     cmp     word [es:0],5089h
        jne     .jpeg
        cmp     word [es:2],474Eh
        jne     .jpeg
        cmp     word [es:4],0A0Dh
        jne     .jpeg
        cmp     word [es:6],0A1Ah
        jne     .jpeg
        mov     al,K_PNG
        jmp     .out
.jpeg:  cmp     word [pv_len+2],0
        jne     @f
        cmp     word [pv_len],3
        jb      .scan
@@:     cmp     word [es:0],0D8FFh
        jne     .scan
        cmp     byte [es:2],0FFh
        jne     .scan
        mov     al,K_JPEG
        jmp     .out
.scan:  pop     es
        xor     si,si                   ; every byte: a NUL, or how many control bytes
        mov     [hd_wrapped],0
        xor     cx,cx                   ; control bytes, 32 bits in dx:cx
        xor     dx,dx
.l:     call    head_end
        je      .count
        call    head_byte
        test    al,al
        jz      .binary
        cmp     al,20h
        jae     .l
        cmp     al,9
        je      .l
        cmp     al,0Ah
        je      .l
        cmp     al,0Dh
        je      .l
        cmp     al,0Ch
        je      .l
        cmp     al,1Bh
        je      .l
        add     cx,1
        adc     dx,0
        jmp     .l
.count: ; binary if control × 100 > the length (the count is at most 65536)
        mov     [sn_hi],dx
        mov     ax,cx
        mov     bx,100
        mul     bx                      ; dx:ax, the low word's share
        cmp     [sn_hi],0
        je      @f
        add     dx,100                  ; 65536 × 100: 100 in the high word
@@:     cmp     dx,[pv_len+2]
        ja      .binary
        jb      .text
        cmp     ax,[pv_len]
        ja      .binary
.text:  mov     al,K_TEXT
        ret
.binary:
        mov     al,K_BINARY
        ret
.out:   pop     es
        ret

; the head's first PV_ROWS lines, as preview::text_lines makes them and cut to PV_COLS: tabs to
; the next multiple of four, control characters as '?', UTF-8 in CP437 where it has the
; character ('?' where not), a sequence cut off at the end of the head dropped
text_lines:
        push    es
        push    ds
        pop     es
        xor     si,si
        mov     [hd_wrapped],0
        mov     [u8_bad],0
        mov     [pv_nlines],0
.line:  cmp     [pv_nlines],PV_ROWS
        jae     .done
        call    head_end
        je      .done
        mov     al,[pv_nlines]
        xor     ah,ah
        mov     di,ax
        shl     di,5
        add     di,pv_text
        xor     dx,dx                   ; the column
.ch:    call    head_end
        je      .eol
        call    utf8_next               ; ax = the character
        jc      .eol
        cmp     ax,0Ah
        je      .eol
        cmp     ax,0Dh
        jne     .tab
        call    head_end                ; "\r\n" ends the line; a lone \r is a control
        je      .ctl
        push    es
        mov     es,[lineseg]
        cmp     byte [es:si],0Ah
        pop     es
        jne     .ctl
        call    head_byte
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
        jb      .one
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
.one:   call    .put
        jmp     .ch
.eol:   mov     bl,[pv_nlines]
        xor     bh,bh
        cmp     dx,PV_COLS
        jbe     @f
        mov     dx,PV_COLS
@@:     mov     [pv_tlen+bx],dl
        inc     [pv_nlines]
        jmp     .line
.done:  pop     es
        ret
; al at column dx, if it is within the preview; past it the column only grows as far as tabs
; still need it to (a line of 64 KiB would wrap a 16-bit count to 0)
.put:   cmp     dx,PV_COLS
        jae     @f
        push    di
        add     di,dx
        mov     [di],al
        pop     di
        inc     dx
        ret
@@:     cmp     dx,4000h
        jae     @f
        inc     dx
@@:     ret

; the character at the head's si → ax, si past it, as String::from_utf8_lossy reads it: each
; maximal invalid subpart is one U+FFFD, shown '?', and so is anything beyond U+FFFF (which
; CP437 has none of). The second byte's range depends on the first (no overlong forms, no
; surrogates, nothing past U+10FFFF). CF=1 when the head ends inside a sequence and nothing
; invalid came before: then text_lines stops there, as preview::text_lines does.
utf8_next:
        call    head_byte
        xor     ah,ah
        cmp     al,80h
        jb      .r
        mov     word [u8_lo],0BF80h     ; lo 80, hi BF
        mov     cl,1
        mov     bx,1Fh
        cmp     al,0C2h
        jb      .bad
        cmp     al,0E0h
        jb      .seq
        mov     cl,2
        mov     bx,0Fh
        cmp     al,0E0h
        jne     @f
        mov     byte [u8_lo],0A0h       ; E0 A0..BF
@@:     cmp     al,0EDh
        jne     @f
        mov     byte [u8_lo+1],9Fh      ; ED 80..9F
@@:     cmp     al,0F0h
        jb      .seq
        mov     cl,3
        mov     bx,07h
        cmp     al,0F0h
        jne     @f
        mov     byte [u8_lo],90h        ; F0 90..BF
@@:     cmp     al,0F4h
        jb      .seq
        ja      .bad
        mov     byte [u8_lo+1],8Fh      ; F4 80..8F
.seq:   mov     [u8_len],cl
        and     bx,ax                   ; the lead byte's bits
.more:  call    head_end
        je      .cut
        push    es
        mov     es,[lineseg]
        mov     al,[es:si]
        pop     es
        cmp     al,[u8_lo]
        jb      .bad                    ; this one is not consumed: it starts afresh
        cmp     al,[u8_lo+1]
        ja      .bad
        mov     word [u8_lo],0BF80h     ; the rest: any continuation byte
        call    head_byte
        and     ax,3Fh
        push    cx
        mov     cl,6
        shl     bx,cl
        pop     cx
        or      bx,ax
        dec     cl
        jnz     .more
        mov     ax,bx
        cmp     [u8_len],3              ; four bytes: beyond U+FFFF
        jne     .r
        mov     ax,'?'
        jmp     .r
.bad:   mov     [u8_bad],1
        mov     ax,'?'
.r:     clc
        ret
.cut:   cmp     [u8_bad],0              ; cut off at the end: dropped, unless the head was
        jne     .bad                    ; already invalid (then the whole is read lossily)
        stc
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

; dx:ax ← the next four bytes, big-endian
rd_be32:
        call    rd_be16
        mov     dx,ax
        ; and the low word
rd_be16:
        call    rd_byte
        mov     ah,al
        call    rd_byte
        ret

; skip dx:cx bytes
rd_skip:
@@:     mov     ax,cx
        or      ax,dx
        jz      .r
        call    rd_byte
        sub     cx,1
        sbb     dx,0
        jmp     @b
.r:     ret

; give up decoding: ax = why (a zero-terminated string), or 0 when a key interrupted
dec_fail:
        mov     [dec_err],ax
        mov     sp,[dec_sp]
        stc
        ret

; decode the pending picture (PNG.ASM or JPEG.ASM) into blocks, or say why not
pv_decode:
        push    es
        pusha
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
        push    ds
        pop     es
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
@@:     call    append_z
        mov     si,[dec_err]
        call    append_z
        mov     byte [di],0
        mov     [pv_state],PV_INFO
        jmp     .out
.openerr:
        mov     si,s_unreadable
        call    pv_info_doserr
.out:   popa
        pop     es
        ret
.run:   mov     [dec_sp],sp
        push    ds
        pop     es
        cmp     [pv_kind],K_PNG
        je      png_decode
        jmp     jpeg_decode

; the picture's sides, at most PV_MAXSIDE each: img_w, img_h words
pv_check_size:
        mov     ax,[img_w]
        dec     ax
        cmp     ax,PV_MAXSIDE
        jae     .big
        mov     ax,[img_h]
        dec     ax
        cmp     ax,PV_MAXSIDE
        jae     .big
        ret
.big:   mov     ax,s_too_large
        jmp     dec_fail

; ---------------------------------------------------------------------------------------------
; the half-block picture

; img_w x img_h → acc_cols x acc_hrows, the sampling steps, the sums cleared. Fitted into
; PV_COLS*8 by PV_ROWS*16 pixels (cells are 8x16) as the image crate's resize_dimensions does it,
; in doubles: ratio = min(nw / w, nh / h), each side (side × ratio).round(), at least 1; never
; scaled up. A block pixel is half a cell: 8 by 8 of those pixels.
acc_setup:
        push    es
        push    ds
        pop     es
        mov     ax,[img_w]
        mov     [fit_w],ax
        mov     ax,[img_h]
        mov     [fit_h],ax
        cmp     word [img_w],PV_COLS*8
        ja      .scale
        cmp     word [img_h],PV_ROWS*16
        jbe     .sized
.scale: mov     ax,PV_COLS*8
        mov     bx,f_t1
        call    f_from_u16
        mov     ax,[img_w]
        mov     bx,f_t2
        call    f_from_u16
        mov     si,f_t1
        mov     di,f_t2
        mov     bx,f_t1
        call    f_div                   ; wratio
        mov     ax,PV_ROWS*16
        mov     bx,f_t2
        call    f_from_u16
        mov     ax,[img_h]
        mov     bx,f_unit
        call    f_from_u16
        mov     si,f_t2
        mov     di,f_unit
        mov     bx,f_t2
        call    f_div                   ; hratio
        mov     si,f_t2
        mov     di,f_t1
        call    f_cmp
        cmp     al,F_LESS
        jne     @f
        mov     si,f_t2                 ; the smaller
        mov     bx,f_t1
        call    f_copy
@@:     mov     ax,[img_w]
        call    .side
        mov     [fit_w],ax
        mov     ax,[img_h]
        call    .side
        mov     [fit_h],ax
.sized: mov     ax,[fit_w]
        add     ax,7
        mov     cl,3
        shr     ax,cl
        cmp     ax,PV_COLS
        jbe     @f
        mov     ax,PV_COLS
@@:     mov     [acc_cols],ax
        mov     ax,[fit_h]
        add     ax,7
        shr     ax,cl
        cmp     ax,PV_HROWS
        jbe     @f
        mov     ax,PV_HROWS
@@:     mov     [acc_hrows],ax
        ; sampling: about eight source pixels a side for each block pixel is plenty
        mov     ax,[img_w]
        mov     bx,[acc_cols]
        call    .step
        mov     [step_x],ax
        mov     ax,[img_h]
        mov     bx,[acc_hrows]
        call    .step
        mov     [step_y],ax
        mov     di,acc
        xor     ax,ax
        mov     cx,PV_COLS*PV_HROWS*ACC_SIZE/2
        rep     stosw
        pop     es
        ret
; ax = a side → ax = (side × ratio in f_t1).round(), at least 1
.side:  mov     bx,f_t2
        call    f_from_u16
        mov     si,f_t2
        mov     di,f_t1
        mov     bx,f_t2
        call    f_mul
        mov     si,f_t2
        call    f_round_u16
        test    ax,ax
        jnz     @f
        inc     ax
@@:     ret
; ax = a side, bx = its block pixels → ax = max(1, side / (bx × 8))
.step:  push    cx
        mov     cl,3
        shl     bx,cl
        pop     cx
        xor     dx,dx
        div     bx
        test    ax,ax
        jnz     @f
        inc     ax
@@:     ret

; ax = y: the block row it falls in, for acc_add
acc_row:
        push    dx
        mul     word [acc_hrows]
        div     word [img_h]
        mul     word [acc_cols]
        push    cx
        mov     cl,5
        shl     ax,cl
        pop     cx
        mov     [acc_base],ax
        pop     dx
        ret

; ax = x, px_r px_g px_b px_a: one pixel of the row acc_row chose
acc_add:
        push    bx
        push    cx
        push    dx
        mul     word [acc_cols]
        div     word [img_w]
        mov     cl,5
        shl     ax,cl
        add     ax,[acc_base]
        mov     bx,ax
        mov     al,[px_r]
        xor     ah,ah
        add     [acc+bx],ax
        adc     word [acc+bx+2],0
        mov     al,[px_g]
        add     [acc+bx+4],ax
        adc     word [acc+bx+6],0
        mov     al,[px_b]
        add     [acc+bx+8],ax
        adc     word [acc+bx+10],0
        mov     al,[px_a]
        add     [acc+bx+12],ax
        adc     word [acc+bx+14],0
        add     word [acc+bx+16],1
        adc     word [acc+bx+18],0
        pop     dx
        pop     cx
        pop     bx
        ret

; jx, jy, cl = channel (0 Y, 1 Cb, 2 Cr), ax = value (0-255), dx = weight: one JPEG block's mean
acc_add_c:
        push    ax
        push    bx
        push    cx
        push    dx
        push    si
        mov     [ac_v],ax
        mov     [ac_w],dx
        mov     ax,[jy]
        mul     word [acc_hrows]
        div     word [img_h]
        mul     word [acc_cols]
        mov     si,ax
        mov     ax,[jx]
        mul     word [acc_cols]
        div     word [img_w]
        add     ax,si
        push    cx
        mov     cl,5
        shl     ax,cl
        pop     cx
        mov     bx,ax
        xor     ch,ch
        shl     cx,1
        shl     cx,1
        add     bx,cx                   ; the channel's sum, 4 bytes on
        mov     ax,[ac_v]
        mul     word [ac_w]
        add     [acc+bx],ax
        adc     [acc+bx+2],dx
        mov     ax,[ac_w]
        add     [acc+bx+16],ax
        adc     word [acc+bx+18],0
        pop     si
        pop     dx
        pop     cx
        pop     bx
        pop     ax
        ret

; dx:ax / (the 32-bit count at [bx+2]:[bx]) → ax, rounded to nearest; the count is not 0 and
; the quotient under 65536. Both are halved until the count fits in 16 bits.
div_count:
        push    cx
        push    di
        mov     cx,[bx]
        mov     di,[bx+2]
@@:     test    di,di
        jz      @f
        shr     di,1
        rcr     cx,1
        shr     dx,1
        rcr     ax,1
        jmp     @b
@@:     push    cx                      ; + half the count, then the division
        shr     cx,1
        add     ax,cx
        adc     dx,0
        pop     cx
        div     cx
        pop     di
        pop     cx
        ret

; the sums → tcol (r, g, b, opaque), then the palette and the colour of each block pixel (blk)
acc_finish:
        push    es
        push    ds
        pop     es
        mov     ax,[acc_cols]
        mul     word [acc_hrows]
        mov     [acc_n],ax
        xor     si,si                   ; acc
        xor     di,di                   ; tcol
        mov     cx,[acc_n]
.px:    mov     byte [tcol+di+3],0
        mov     ax,word [acc+si+16]
        or      ax,word [acc+si+18]
        jz      .next
        lea     bx,[acc+si+16]
        cmp     [acc_mode],0
        jne     .ycc
        mov     ax,word [acc+si+12]     ; alpha: under half, transparent
        mov     dx,word [acc+si+14]
        call    div_count
        cmp     ax,128
        jb      .next
        mov     ax,word [acc+si]
        mov     dx,word [acc+si+2]
        call    div_count
        mov     [tcol+di],al
        mov     ax,word [acc+si+4]
        mov     dx,word [acc+si+6]
        call    div_count
        mov     [tcol+di+1],al
        mov     ax,word [acc+si+8]
        mov     dx,word [acc+si+10]
        call    div_count
        mov     [tcol+di+2],al
        mov     byte [tcol+di+3],1
        jmp     .next
.ycc:   ; Y, Cb, Cr: each over its own weights (the channels of a subsampled picture differ)
        push    cx
        xor     cx,cx
.ch:    mov     bx,cx
        mov     word [ycc+bx],128
        push    si
        add     si,cx
        mov     ax,word [acc+si+16]
        or      ax,word [acc+si+18]
        jz      @f
        lea     bx,[acc+si+16]
        mov     ax,word [acc+si]
        mov     dx,word [acc+si+2]
        call    div_count
        mov     bx,cx
        mov     [ycc+bx],ax
@@:     pop     si
        add     cx,4
        cmp     cx,12
        jb      .ch
        pop     cx
        call    ycc_rgb
        mov     byte [tcol+di+3],1
.next:  add     si,ACC_SIZE
        add     di,4
        dec     cx
        jnz     .px
        call    pal_choose
        pop     es
        ret

; ycc (Y, Cb, Cr words at 0, 4, 8) → tcol+di: R = Y + 1.402 (Cr-128), G = Y - 0.344 (Cb-128)
; - 0.714 (Cr-128), B = Y + 1.772 (Cb-128), in 1/128ths, clamped
ycc_rgb:
        push    bx
        push    cx
        push    dx
        mov     ax,[ycc+8]
        sub     ax,128
        mov     bx,179                  ; 1.402 × 128
        imul    bx
        call    .add_y
        mov     [tcol+di],al
        mov     ax,[ycc+4]
        sub     ax,128
        mov     bx,-44                  ; -0.344 × 128
        imul    bx
        mov     cx,ax
        mov     ax,[ycc+8]
        sub     ax,128
        mov     bx,-91                  ; -0.714 × 128
        imul    bx
        add     ax,cx
        call    .add_y
        mov     [tcol+di+1],al
        mov     ax,[ycc+4]
        sub     ax,128
        mov     bx,227                  ; 1.772 × 128
        imul    bx
        call    .add_y
        mov     [tcol+di+2],al
        pop     dx
        pop     cx
        pop     bx
        ret
; ax (1/128ths, signed, within 16 bits) → al = Y + ax/128 rounded to nearest, clamped to 0..255
.add_y: add     ax,64
        sar     ax,1
        sar     ax,1
        sar     ax,1
        sar     ax,1
        sar     ax,1
        sar     ax,1
        sar     ax,1
        add     ax,[ycc]
        jns     @f
        xor     ax,ax
@@:     cmp     ax,255
        jbe     @f
        mov     ax,255
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
        mov     bl,al
        xor     bh,bh
        mov     ax,1015h
        int     10h
        pop     bx
        push    bx
        mov     ax,bx
        shl     bx,1
        add     bx,ax
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
        shl     al,1
        shl     al,1
        shr     ah,1
        shr     ah,1
        shr     ah,1
        shr     ah,1
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
        mov     word [far_d],0
        mov     word [far_d+2],0
        mov     [far_i],-1
        xor     di,di
.f:     cmp     di,[acc_n]
        jae     .fd
        mov     bx,di
        shl     bx,1
        shl     bx,1
        cmp     byte [tcol+bx+3],0
        je      .fn
        call    nearest_in              ; the interface's colours and the slots chosen so far
        cmp     [far_i],-1
        je      .take
        cmp     dx,[far_d+2]
        jb      .fn
        ja      .take
        cmp     ax,[far_d]
        jbe     .fn
.take:  mov     [far_d],ax
        mov     [far_d+2],dx
        mov     [far_i],di
.fn:    inc     di
        jmp     .f
.fd:    cmp     [far_i],-1
        je      .refine
        mov     ax,[far_d]
        or      ax,[far_d+2]
        jz      .refine                 ; every pixel has its colour already
        mov     bx,[far_i]
        shl     bx,1
        shl     bx,1
        mov     si,bp                   ; not [bp]: that is the stack's segment
        mov     al,[adaptive+si]
        xor     ah,ah
        mov     si,ax
        shl     si,1
        add     si,ax
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
        xor     ax,ax
        mov     cx,ADAPT_N*8
        rep     stosw
        xor     di,di
.a:     cmp     di,[acc_n]
        jae     .means
        mov     bx,di
        shl     bx,1
        shl     bx,1
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
@@:     push    cx
        mov     cl,4
        shl     si,cl
        pop     cx
        mov     al,[tcol+bx]
        xor     ah,ah
        add     [rsum+si],ax
        adc     word [rsum+si+2],0
        mov     al,[tcol+bx+1]
        add     [rsum+si+4],ax
        adc     word [rsum+si+6],0
        mov     al,[tcol+bx+2]
        add     [rsum+si+8],ax
        adc     word [rsum+si+10],0
        add     word [rsum+si+12],1
.an:    inc     di
        jmp     .a
.means: xor     si,si
.m:     cmp     si,[adapt_used]
        jae     .assign
        mov     bx,si
        push    cx
        mov     cl,4
        shl     bx,cl
        pop     cx
        mov     cx,[rsum+bx+12]
        jcxz    .mn
        mov     al,[adaptive+si]
        xor     ah,ah
        mov     di,ax
        shl     di,1
        add     di,ax
        mov     ax,[rsum+bx]
        mov     dx,[rsum+bx+2]
        div     cx
        mov     [pal_rgb+di],al
        mov     ax,[rsum+bx+4]
        mov     dx,[rsum+bx+6]
        div     cx
        mov     [pal_rgb+di+1],al
        mov     ax,[rsum+bx+8]
        mov     dx,[rsum+bx+10]
        div     cx
        mov     [pal_rgb+di+2],al
.mn:    inc     si
        jmp     .m
.assign:
        xor     di,di
.b:     cmp     di,[acc_n]
        jae     .r
        mov     bx,di
        shl     bx,1
        shl     bx,1
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

; tcol+bx → dl = the nearest of all 16 colours
nearest:
        push    bp
        mov     bp,ADAPT_N
        call    nearest_in
        mov     dl,[near_c]
        pop     bp
        ret

; tcol+bx → near_c, the nearest among the interface's colours and the first bp adaptive ones,
; and dx:ax how far: 2 dr² + 4 dg² + 3 db²
nearest_in:
        push    cx
        push    si
        push    di
        push    bx
        mov     word [near_d],0FFFFh
        mov     word [near_d+2],0FFFFh
        xor     cx,cx
.c:     xor     si,si                   ; an adaptive slot not yet chosen does not count
@@:     cmp     si,ADAPT_N
        jae     .ok
        cmp     cl,[adaptive+si]
        je      @f
        inc     si
        jmp     @b
@@:     cmp     si,bp
        jae     .n
.ok:    mov     si,cx
        shl     si,1
        add     si,cx
        mov     word [dist_sum],0
        mov     word [dist_sum+2],0
        mov     di,0                    ; each channel with its weight
.chan:  push    bx
        push    si
        add     bx,di                   ; (no si+di address on a 286)
        add     si,di
        mov     al,[tcol+bx]
        xor     ah,ah
        mov     bl,[pal_rgb+si]
        xor     bh,bh
        sub     ax,bx
        imul    ax                      ; its square: dx:ax, under 65536
        mov     bl,[dist_weight+di]
        xor     bh,bh
        mul     bx
        add     [dist_sum],ax
        adc     [dist_sum+2],dx
        pop     si
        pop     bx
        inc     di
        cmp     di,3
        jb      .chan
        mov     ax,[dist_sum]
        mov     dx,[dist_sum+2]
        cmp     dx,[near_d+2]
        jb      .better
        ja      .n
        cmp     ax,[near_d]
        jae     .n
.better:
        mov     [near_d],ax
        mov     [near_d+2],dx
        mov     [near_c],cl
.n:     inc     cx
        cmp     cx,16
        jb      .c
        mov     ax,[near_d]
        mov     dx,[near_d+2]
        pop     bx
        pop     di
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
.l:     mov     bl,[adaptive+di]
        xor     bh,bh
        push    bx
        mov     ax,bx
        shl     bx,1
        add     bx,ax
        mov     dh,[si+bx]
        mov     ch,[si+bx+1]
        mov     cl,[si+bx+2]
        shr     dh,1
        shr     dh,1
        shr     ch,1
        shr     ch,1
        shr     cl,1
        shr     cl,1
        pop     bx
        mov     bl,[dacidx+bx]
        xor     bh,bh
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
        sub     ax,[acc_cols]
        shr     ax,1
        add     dl,al
        xor     bx,bx                   ; block row pair
.row:   mov     ax,bx
        shl     ax,1
        cmp     ax,[acc_hrows]
        jae     .r
        xor     cx,cx
.col:   cmp     cx,[acc_cols]
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
        cmp     ax,[acc_hrows]
        pop     ax
        jae     @f
        add     si,[acc_cols]
        mov     ah,[blk+si]
@@:     pop     dx
        push    dx
        add     dl,cl
        add     dh,bl
        cmp     al,0FFh
        je      .noup
        cmp     ah,0FFh
        je      .uponly
        shl     ah,1                    ; ▀ in the upper colour over the lower one
        shl     ah,1
        shl     ah,1
        shl     ah,1
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
