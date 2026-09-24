; TESTFP.ASM: SOFTFP.ASM checked against the host's IEEE doubles. tests/softfp.py writes FPIN.BIN
; (records: op byte, k word, a and b as IEEE doubles), this runs each through SOFTFP.ASM and
; writes FPOUT.BIN (a double, or a word for cmp and the roundings); softfp.py compares them.
include '..\J286.INC'
        org     100h
        cld
        mov     ax,3D00h
        mov     dx,fin
        int     21h
        jc      done
        mov     [hin],ax
        mov     ah,3Ch
        xor     cx,cx
        mov     dx,fout
        int     21h
        jc      done
        mov     [hout],ax
next:   mov     ah,3Fh
        mov     bx,[hin]
        mov     cx,19
        mov     dx,rec
        int     21h
        cmp     ax,19
        jne     done
        mov     si,rec+3
        mov     bx,fa
        call    unpack
        mov     si,rec+11
        mov     bx,fb
        call    unpack
        mov     al,[rec]
        mov     si,fa
        mov     di,fb
        mov     bx,fc
        cmp     al,0
        jne     @f
        call    f_add
        jmp     .double
@@:     cmp     al,1
        jne     @f
        call    f_sub
        jmp     .double
@@:     cmp     al,2
        jne     @f
        call    f_mul
        jmp     .double
@@:     cmp     al,3
        jne     @f
        call    f_div
        jmp     .double
@@:     cmp     al,4
        jne     @f
        call    f_cmp
        xor     ah,ah
        jmp     .word
@@:     cmp     al,5
        jne     @f
        call    f_round_u16
        jmp     .word
@@:     cmp     al,6
        jne     @f
        mov     cx,word [rec+1]
        call    f_round_even
        test    dx,dx                   ; the test compares 16 bits
        jz      .word
        mov     ax,0FFFFh
        jmp     .word
@@:     mov     si,rec+3                ; 7: the first 8 bytes as an unsigned integer
        mov     bx,fc
        call    f_from_u64
.double:
        mov     si,fc
        mov     di,res
        call    pack
        mov     cx,8
        jmp     .write
.word:  mov     word [res],ax
        mov     cx,2
.write: mov     ah,40h
        mov     bx,[hout]
        mov     dx,res
        int     21h
        jmp     next
done:   mov     ah,3Eh
        mov     bx,[hout]
        int     21h
        mov     ax,4C00h
        int     21h

; the double at [si] → the unpacked value at [bx]
unpack:
        mov     ax,[si+6]
        mov     dl,ah
        shr     dl,7
        mov     [bx+F_S],dl
        mov     dx,ax
        and     dx,7FF0h
        mov     cl,4
        shr     dx,cl                   ; the biased exponent
        mov     di,bx
        mov     ax,[si]
        mov     [di],ax
        mov     ax,[si+2]
        mov     [di+2],ax
        mov     ax,[si+4]
        mov     [di+4],ax
        mov     ax,[si+6]
        and     ax,000Fh
        mov     [di+6],ax
        cmp     dx,7FFh
        je      .special
        test    dx,dx
        jz      .zero
        or      word [di+6],0010h       ; the hidden bit, then 11 places left
        mov     cx,11
@@:     shl     word [di],1
        rcl     word [di+2],1
        rcl     word [di+4],1
        rcl     word [di+6],1
        loop    @b
        sub     dx,1023
        mov     [bx+F_E],dx
        mov     byte [bx+F_C],FC_NORMAL
        ret
.zero:  mov     byte [bx+F_C],FC_ZERO   ; subnormals are not tried
        ret
.special:
        mov     ax,[di]
        or      ax,[di+2]
        or      ax,[di+4]
        or      ax,[di+6]
        mov     byte [bx+F_C],FC_INF
        jz      @f
        mov     byte [bx+F_C],FC_NAN
@@:     ret

; the unpacked value at [si] → the double at [di]
pack:
        mov     al,[si+F_C]
        cmp     al,FC_NORMAL
        je      .normal
        mov     word [di],0
        mov     word [di+2],0
        mov     word [di+4],0
        mov     word [di+6],0
        cmp     al,FC_ZERO
        je      .sign
        mov     word [di+6],7FF0h
        cmp     al,FC_INF
        je      .sign
        mov     word [di+6],7FF8h
        ret
.normal:
        mov     ax,[si]
        mov     [di],ax
        mov     ax,[si+2]
        mov     [di+2],ax
        mov     ax,[si+4]
        mov     [di+4],ax
        mov     ax,[si+6]
        mov     [di+6],ax
        mov     cx,11
@@:     shr     word [di+6],1
        rcr     word [di+4],1
        rcr     word [di+2],1
        rcr     word [di],1
        loop    @b
        and     word [di+6],000Fh
        mov     ax,[si+F_E]
        add     ax,1023
        mov     cl,4
        shl     ax,cl
        or      [di+6],ax
.sign:  cmp     byte [si+F_S],0
        je      @f
        or      byte [di+7],80h
@@:     ret

include '..\SOFTFP.ASM'

fin     db 'FPIN.BIN',0
fout    db 'FPOUT.BIN',0
hin     dw 0
hout    dw 0
rec     rb 19
res     rb 8
fa      rb F_SIZE
fb      rb F_SIZE
fc      rb F_SIZE
include '..\FPDATA.ASM'
