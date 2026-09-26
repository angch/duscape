; SOFTFP.ASM, included by DUSCAPE.ASM: IEEE 754 double precision in 16-bit code, for a 286
; with no coprocessor. The treemap must lay out exactly as the Rust one does, and Rust computes
; it in f64, so every result here is the double Rust would get: rounded to 53 bits, to nearest,
; ties to even, after each operation.
;
; A value is kept unpacked, F_SIZE bytes:
;   F_M   4 words, the significand, least significant first, its top bit set (low 11 bits 0)
;   F_E   word, the exponent: the value is (F_M / 2^63) * 2^F_E
;   F_S   byte, 1 if negative
;   F_C   byte, the class: FC_ZERO, FC_NORMAL, FC_INF or FC_NAN
; so nothing is packed or unpacked. The exponent is not bounded: results that IEEE would take
; to infinity, to zero or to a subnormal stay normal here. None arise in this program (its values
; are sizes, shares of them and screen coordinates), and everything else is exact.
;
; The operations take ds:si and ds:di and write ds:bx (which may be either); they keep every
; register but AX and the flags.

F_M             = 0
F_E             = 8
F_S             = 10
F_C             = 11
F_SIZE          = 12

FC_ZERO         = 0
FC_NORMAL       = 1
FC_INF          = 2
FC_NAN          = 3

; f_cmp's answers, in al
F_LESS          = 0
F_EQUAL         = 1
F_GREATER       = 2
F_UNORDERED     = 3

; [bx] ← [si]
f_copy:
        push    cx
        push    si
        push    di
        push    es
        push    ds
        pop     es
        mov     di,bx
        mov     cx,F_SIZE/2
        rep     movsw
        pop     es
        pop     di
        pop     si
        pop     cx
        ret

; [bx] ← ax
f_from_u16:
        push    si
        mov     si,fz_int
        mov     [si],ax
        mov     word [si+2],0
        mov     word [si+4],0
        mov     word [si+6],0
        call    f_from_u64
        pop     si
        ret

; [bx] ← the unsigned 64-bit integer at [si], rounded
f_from_u64:
        pusha
        mov     ax,[si]
        mov     [fz+2],ax               ; fz: 5 words, the significand over fz+2..fz+9
        mov     ax,[si+2]
        mov     [fz+4],ax
        mov     ax,[si+4]
        mov     [fz+6],ax
        mov     ax,[si+6]
        mov     [fz+8],ax
        mov     word [fz],0
        mov     ax,[fz+2]
        or      ax,[fz+4]
        or      ax,[fz+6]
        or      ax,[fz+8]
        jnz     .some
        mov     byte [bx+F_C],FC_ZERO
        mov     byte [bx+F_S],0
        mov     word [bx+F_E],0
        popa
        ret
.some:  mov     word [fz_e],63
        mov     byte [fz_s],0
        call    fz_normalise
        call    fz_round
        call    fz_store
        popa
        ret

; [bx] ← [si] + [di]
f_add:
        pusha
        mov     al,[di+F_S]
        mov     [fb_s],al
        jmp     f_addsub
; [bx] ← [si] - [di]
f_sub:
        pusha
        mov     al,[di+F_S]
        xor     al,1
        mov     [fb_s],al
f_addsub:                               ; fb_s: the sign [di] is taken with
        mov     al,[si+F_C]
        mov     ah,[di+F_C]
        cmp     al,FC_NAN
        je      .nan
        cmp     ah,FC_NAN
        je      .nan
        cmp     al,FC_INF
        jne     .ainf
        cmp     ah,FC_INF
        jne     .copya
        mov     dl,[si+F_S]
        cmp     dl,[fb_s]
        jne     .nan                    ; inf - inf
        jmp     .copya
.ainf:  cmp     ah,FC_INF
        je      .copyb
        cmp     al,FC_ZERO
        jne     .azero
        cmp     ah,FC_ZERO
        jne     .copyb
        mov     dl,[si+F_S]             ; -0 + -0 is -0, anything else +0
        and     dl,[fb_s]
        mov     [bx+F_S],dl
        mov     byte [bx+F_C],FC_ZERO
        mov     word [bx+F_E],0
        popa
        ret
.azero: cmp     ah,FC_ZERO
        je      .copya
        ; both normal: [si] the one with the larger exponent
        mov     ax,[si+F_E]
        mov     dl,[si+F_S]
        mov     dh,[fb_s]
        cmp     ax,[di+F_E]
        jge     @f
        xchg    si,di
        xchg    dl,dh
        mov     ax,[si+F_E]
@@:     mov     [fz_e],ax
        mov     [fz_s],dl
        mov     [fy_s],dh
        sub     ax,[di+F_E]
        mov     cx,ax                   ; how far [di] is shifted right
        ; fz = [si] and fy = [di], 80 bits each, one guard word under the significand
        push    di
        mov     di,fz
        call    fz_load
        pop     si
        mov     di,fy
        call    fz_load
        mov     si,fy
        call    fx_shr_sticky           ; fy >>= cx, the lost bits into its lowest bit
        mov     al,[fz_s]
        cmp     al,[fy_s]
        jne     .subtract
        ; the same sign: add the magnitudes
        mov     si,fz
        mov     di,fy
        call    fx_add                  ; CF: carried out of the top
        jnc     .round
        mov     si,fz
        call    fx_shr1_carry
        inc     word [fz_e]
        jmp     .round
.subtract:
        mov     si,fz
        mov     di,fy
        call    fx_cmp
        jae     @f
        call    fx_swap                 ; |[di]| is bigger (same exponents): it gives the sign
        mov     al,[fy_s]
        mov     [fz_s],al
@@:     mov     si,fz
        mov     di,fy
        call    fx_sub
        mov     ax,[fz]
        or      ax,[fz+2]
        or      ax,[fz+4]
        or      ax,[fz+6]
        or      ax,[fz+8]
        jnz     .norm
        mov     byte [bx+F_C],FC_ZERO   ; x - x is +0
        mov     byte [bx+F_S],0
        mov     word [bx+F_E],0
        popa
        ret
.norm:  call    fz_normalise
.round: call    fz_round
        call    fz_store
        popa
        ret
.copya: call    f_copy
        popa
        ret
.copyb: mov     si,di
        call    f_copy
        mov     al,[fb_s]
        mov     [bx+F_S],al
        popa
        ret
.nan:   mov     byte [bx+F_C],FC_NAN
        mov     byte [bx+F_S],0
        popa
        ret

; [bx] ← [si] × [di]
f_mul:
        pusha
        mov     dl,[si+F_S]
        xor     dl,[di+F_S]
        mov     [fz_s],dl
        mov     al,[si+F_C]
        mov     ah,[di+F_C]
        cmp     al,FC_NAN
        je      .nan
        cmp     ah,FC_NAN
        je      .nan
        cmp     al,FC_INF
        je      .inf
        cmp     ah,FC_INF
        je      .inf
        cmp     al,FC_ZERO
        je      .zero
        cmp     ah,FC_ZERO
        je      .zero
        mov     ax,[si+F_E]
        add     ax,[di+F_E]
        mov     [fz_e],ax
        ; the 128-bit product, 16 words by 16
        push    bx
        mov     cx,8
        push    di
        mov     di,fp
        xor     ax,ax
        push    es
        push    ds
        pop     es
        rep     stosw
        pop     es
        pop     di
        xor     bx,bx                   ; i, a word of [si]
.i:     push    bx
        mov     bp,bx                   ; j, a word of [di], with the product word i+j
        xor     cx,cx                   ; the carry into word i+j
        push    si
        add     si,bx
        mov     ax,[si]
        pop     si
        mov     [fm_a],ax
        xor     bp,bp
.j:     push    di
        add     di,bp
        mov     ax,[di]
        pop     di
        mul     word [fm_a]             ; dx:ax
        push    bx
        add     bx,bp
        add     ax,cx
        adc     dx,0
        add     [fp+bx],ax
        adc     dx,0
        mov     cx,dx
        pop     bx
        add     bp,2
        cmp     bp,8
        jb      .j
        push    bx
        add     bx,8
        mov     [fp+bx],cx
        pop     bx
        pop     bx
        add     bx,2
        cmp     bx,8
        jb      .i
        pop     bx
        ; normalise: the top bit is 127 or 126; keep 80 bits under it, the rest as sticky
        test    byte [fp+15],80h
        jnz     .top
        mov     si,fp
        mov     cx,8
        clc
@@:     rcl     word [si],1
        inc     si
        inc     si
        loop    @b
        jmp     .take
.top:   inc     word [fz_e]
.take:  mov     ax,[fp+6]
        mov     [fz],ax
        mov     ax,[fp+8]
        mov     [fz+2],ax
        mov     ax,[fp+10]
        mov     [fz+4],ax
        mov     ax,[fp+12]
        mov     [fz+6],ax
        mov     ax,[fp+14]
        mov     [fz+8],ax
        mov     ax,[fp]
        or      ax,[fp+2]
        or      ax,[fp+4]
        jz      @f
        or      byte [fz],1
@@:     call    fz_round
        call    fz_store
        popa
        ret
.inf:   cmp     al,FC_ZERO              ; inf × 0
        je      .nan
        cmp     ah,FC_ZERO
        je      .nan
        mov     byte [bx+F_C],FC_INF
        mov     [bx+F_S],dl
        popa
        ret
.zero:  mov     byte [bx+F_C],FC_ZERO
        mov     [bx+F_S],dl
        mov     word [bx+F_E],0
        popa
        ret
.nan:   mov     byte [bx+F_C],FC_NAN
        mov     byte [bx+F_S],0
        popa
        ret

; [bx] ← [si] / [di]
f_div:
        pusha
        mov     dl,[si+F_S]
        xor     dl,[di+F_S]
        mov     [fz_s],dl
        mov     al,[si+F_C]
        mov     ah,[di+F_C]
        cmp     al,FC_NAN
        je      .nan
        cmp     ah,FC_NAN
        je      .nan
        cmp     al,FC_INF
        jne     .ainf
        cmp     ah,FC_INF
        je      .nan                    ; inf / inf
        jmp     .inf
.ainf:  cmp     ah,FC_INF
        je      .zero                   ; x / inf
        cmp     al,FC_ZERO
        jne     .azero
        cmp     ah,FC_ZERO
        je      .nan                    ; 0 / 0
        jmp     .zero
.azero: cmp     ah,FC_ZERO
        je      .inf                    ; x / 0
        mov     ax,[si+F_E]
        sub     ax,[di+F_E]
        mov     [fz_e],ax
        ; r (5 words, room for the shift) = [si], d = [di]
        mov     ax,[si]
        mov     [fr],ax
        mov     ax,[si+2]
        mov     [fr+2],ax
        mov     ax,[si+4]
        mov     [fr+4],ax
        mov     ax,[si+6]
        mov     [fr+6],ax
        mov     word [fr+8],0
        mov     ax,[di]
        mov     [fd],ax
        mov     ax,[di+2]
        mov     [fd+2],ax
        mov     ax,[di+4]
        mov     [fd+4],ax
        mov     ax,[di+6]
        mov     [fd+6],ax
        mov     word [fd+8],0
        call    .ge
        jae     @f
        call    .shl_r                  ; r < d: the quotient would be under 1
        dec     word [fz_e]
@@:     mov     di,fz
        push    es
        push    ds
        pop     es
        xor     ax,ax
        mov     cx,5
        rep     stosw
        pop     es
        mov     cx,80                   ; 80 quotient bits, the top one set
.bit:   call    .ge
        cmc                             ; CF = r >= d
        jnc     .zero_bit
        mov     ax,[fd]                 ; r -= d
        sub     [fr],ax
        mov     ax,[fd+2]
        sbb     [fr+2],ax
        mov     ax,[fd+4]
        sbb     [fr+4],ax
        mov     ax,[fd+6]
        sbb     [fr+6],ax
        mov     ax,[fd+8]
        sbb     [fr+8],ax
        stc
.zero_bit:
        rcl     word [fz],1             ; q = q << 1 | bit
        rcl     word [fz+2],1
        rcl     word [fz+4],1
        rcl     word [fz+6],1
        rcl     word [fz+8],1
        call    .shl_r
        loop    .bit
        mov     ax,[fr]                 ; a remainder: sticky
        or      ax,[fr+2]
        or      ax,[fr+4]
        or      ax,[fr+6]
        or      ax,[fr+8]
        jz      @f
        or      byte [fz],1
@@:     call    fz_round
        call    fz_store
        popa
        ret
.inf:   mov     byte [bx+F_C],FC_INF
        mov     [bx+F_S],dl
        popa
        ret
.zero:  mov     byte [bx+F_C],FC_ZERO
        mov     [bx+F_S],dl
        mov     word [bx+F_E],0
        popa
        ret
.nan:   mov     byte [bx+F_C],FC_NAN
        mov     byte [bx+F_S],0
        popa
        ret
; CF=1 if r < d (80 bits each)
.ge:    push    si
        mov     si,8
@@:     mov     ax,[fr+si]
        cmp     ax,[fd+si]
        jne     @f
        sub     si,2
        jnc     @b
        clc                             ; equal: not below
@@:     pop     si
        ret
.shl_r: shl     word [fr],1
        rcl     word [fr+2],1
        rcl     word [fr+4],1
        rcl     word [fr+6],1
        rcl     word [fr+8],1
        ret

; al ← how [si] compares with [di]: F_LESS, F_EQUAL, F_GREATER or F_UNORDERED
f_cmp:
        push    cx
        push    dx
        push    bx
        mov     al,[si+F_C]
        mov     ah,[di+F_C]
        cmp     al,FC_NAN
        je      .un
        cmp     ah,FC_NAN
        je      .un
        mov     dl,[si+F_S]             ; the signs, a zero counting as +
        cmp     al,FC_ZERO
        jne     @f
        xor     dl,dl
@@:     mov     dh,[di+F_S]
        cmp     ah,FC_ZERO
        jne     @f
        xor     dh,dh
@@:     cmp     dl,dh
        je      .same
        mov     al,F_LESS               ; negative below positive
        test    dl,dl
        jnz     .out
        mov     al,F_GREATER
        jmp     .out
.same:  ; the magnitudes: class first (zero, normal, inf), then exponent, then significand
        cmp     al,ah
        jne     .mag
        cmp     al,FC_NORMAL
        jne     .eq
        mov     cx,[si+F_E]
        cmp     cx,[di+F_E]
        jl      .less
        jg      .greater
        mov     bx,6
@@:     mov     cx,[si+bx]
        cmp     cx,[di+bx]
        jb      .less
        ja      .greater
        sub     bx,2
        jnc     @b
.eq:    mov     al,F_EQUAL
        jmp     .out
.mag:   jb      .less
.greater:
        mov     al,F_GREATER
        jmp     .sign
.less:  mov     al,F_LESS
.sign:  test    dl,dl                   ; both negative: the bigger magnitude is the smaller
        jz      .out
        xor     al,2
        jmp     .out
.un:    mov     al,F_UNORDERED
.out:   pop     bx
        pop     dx
        pop     cx
        ret

; ax ← [si].round() as u16: half away from zero; NaN and anything negative 0; 65535 at most
f_round_u16:
        push    cx
        push    dx
        xor     cx,cx                   ; ties away, × 1
        call    f_round
        test    dx,dx
        jz      @f
        mov     ax,0FFFFh
@@:     pop     dx
        pop     cx
        ret
; dx:ax ← [si] × cx (a small integer, at least 1), rounded half to even as Rust's {:.N} prints
; it, 2^32 - 1 at most; the product is exact, so {:.1} of x is f_round_even with cx = 10
f_round_even:
        push    cx
        or      cx,8000h
        call    f_round
        pop     cx
        ret
; dx:ax ← [si] × (cx & 7FFFh, or 1), rounded: half to even if cx has its top bit, else away
f_round:
        push    bx
        push    cx
        push    si
        push    di
        mov     al,[si+F_C]
        cmp     al,FC_NAN
        je      .zero
        cmp     al,FC_ZERO
        je      .zero
        cmp     byte [si+F_S],0
        jne     .zero
        cmp     al,FC_INF
        je      .max
        mov     [fr_mode],cx
        and     cx,7FFFh
        jnz     @f
        inc     cx                      ; the plain round: × 1
@@:     ; p = significand × cx, exact: 5 words
        xor     dx,dx
        xor     bx,bx
.mul:   mov     ax,[si+bx]
        push    dx
        mul     cx
        pop     di
        add     ax,di
        adc     dx,0
        mov     [fr+bx],ax
        add     bx,2
        cmp     bx,8
        jb      .mul
        mov     [fr+8],dx
        ; the value is p × 2^(e - 63): the integer part is p >> (63 - e)
        mov     cx,63
        sub     cx,[si+F_E]             ; how far right
        jl      .max                    ; e > 63: far too big
        cmp     cx,80
        jae     .zero                   ; below any half
        ; shift p right by cx - 1, so its lowest bit is the half; the bits below, sticky
        xor     dl,dl                   ; sticky
        dec     cx
        js      .exact                  ; cx was 0: no fraction, nothing to round
.sh:    jcxz    .shd
        shr     word [fr+8],1
        rcr     word [fr+6],1
        rcr     word [fr+4],1
        rcr     word [fr+2],1
        rcr     word [fr],1
        adc     dl,0
        loop    .sh
.shd:   mov     al,byte [fr]            ; the half bit
        and     al,1
        shr     word [fr+8],1           ; and the integer part
        rcr     word [fr+6],1
        rcr     word [fr+4],1
        rcr     word [fr+2],1
        rcr     word [fr],1
        test    al,al
        jz      .int
        test    [fr_mode],8000h
        jz      .up                     ; ties away: a half or more goes up
        test    dl,dl
        jnz     .up                     ; more than a half
        test    byte [fr],1
        jz      .int                    ; a tie, to the even one
.up:    add     word [fr],1
        adc     word [fr+2],0
        adc     word [fr+4],0
        adc     word [fr+6],0
        adc     word [fr+8],0
        jmp     .int
.exact:                                 ; e = 63: p is the integer, and too big
.int:   mov     ax,[fr+4]
        or      ax,[fr+6]
        or      ax,[fr+8]
        jnz     .max
        mov     ax,[fr]
        mov     dx,[fr+2]
        jmp     .out
.max:   mov     ax,0FFFFh
        mov     dx,ax
        jmp     .out
.zero:  xor     ax,ax
        xor     dx,dx
.out:   pop     di
        pop     si
        pop     cx
        pop     bx
        ret

; ---------------------------------------------------------------------------------------------
; the working registers: fz and fy, 80 bits each (five words, least significant first; the
; significand in the top four, a guard word under it)

; ds:[si] (a normal value) → ds:[di], 80 bits
fz_load:
        mov     word [di],0
        mov     ax,[si]
        mov     [di+2],ax
        mov     ax,[si+2]
        mov     [di+4],ax
        mov     ax,[si+4]
        mov     [di+6],ax
        mov     ax,[si+6]
        mov     [di+8],ax
        ret

; [si] (80 bits) >>= cx, the bits lost or'ed into its lowest bit
fx_shr_sticky:
        xor     dl,dl
        cmp     cx,80
        jb      .words
        mov     ax,[si]                 ; everything goes: only the sticky bit is left
        or      ax,[si+2]
        or      ax,[si+4]
        or      ax,[si+6]
        or      ax,[si+8]
        mov     word [si],0
        mov     word [si+2],0
        mov     word [si+4],0
        mov     word [si+6],0
        mov     word [si+8],0
        jz      .r
        mov     byte [si],1
.r:     ret
.words: cmp     cx,16
        jb      .bits
        cmp     word [si],0             ; a whole word
        je      @f
        mov     dl,1
@@:     mov     ax,[si+2]
        mov     [si],ax
        mov     ax,[si+4]
        mov     [si+2],ax
        mov     ax,[si+6]
        mov     [si+4],ax
        mov     ax,[si+8]
        mov     [si+6],ax
        mov     word [si+8],0
        sub     cx,16
        jmp     .words
.bits:  jcxz    .done
@@:     shr     word [si+8],1
        rcr     word [si+6],1
        rcr     word [si+4],1
        rcr     word [si+2],1
        rcr     word [si],1
        adc     dl,0
        loop    @b
.done:  test    dl,dl
        jz      .r2
        or      byte [si],1
.r2:    ret

; [si] += [di], 80 bits; CF out
fx_add:
        mov     ax,[di]
        add     [si],ax
        mov     ax,[di+2]
        adc     [si+2],ax
        mov     ax,[di+4]
        adc     [si+4],ax
        mov     ax,[di+6]
        adc     [si+6],ax
        mov     ax,[di+8]
        adc     [si+8],ax
        ret

; [si] -= [di], 80 bits
fx_sub:
        mov     ax,[di]
        sub     [si],ax
        mov     ax,[di+2]
        sbb     [si+2],ax
        mov     ax,[di+4]
        sbb     [si+4],ax
        mov     ax,[di+6]
        sbb     [si+6],ax
        mov     ax,[di+8]
        sbb     [si+8],ax
        ret

; CF=1 if [si] < [di], 80 bits
fx_cmp:
        push    bx
        mov     bx,8
@@:     mov     ax,[si+bx]
        cmp     ax,[di+bx]
        jne     @f
        sub     bx,2
        jnc     @b
        clc
@@:     pop     bx
        ret

fx_swap:
        push    cx
        push    bx
        xor     bx,bx
@@:     mov     ax,[fz+bx]
        xchg    ax,[fy+bx]
        mov     [fz+bx],ax
        add     bx,2
        cmp     bx,10
        jb      @b
        pop     bx
        pop     cx
        ret

; [si] (80 bits) >>= 1 with the carry coming in at the top; the bit lost is kept as sticky
fx_shr1_carry:
        rcr     word [si+8],1
        rcr     word [si+6],1
        rcr     word [si+4],1
        rcr     word [si+2],1
        rcr     word [si],1
        jnc     @f
        or      byte [si],1
@@:     ret

; fz shifted left until its top bit is set, fz_e counting down
fz_normalise:
@@:     test    byte [fz+9],80h
        jnz     @f
        shl     word [fz],1
        rcl     word [fz+2],1
        rcl     word [fz+4],1
        rcl     word [fz+6],1
        rcl     word [fz+8],1
        dec     word [fz_e]
        jmp     @b
@@:     ret

; fz rounded to 53 bits, to nearest, ties to even. The 53 bits are the top of fz+2..fz+9; bit
; 10 of fz+2 is the half, the bits under it (and all of fz) the sticky.
fz_round:
        mov     ax,[fz+2]
        test    ax,0400h
        jz      .cut
        test    ax,03FFh                ; above the half?
        jnz     .up
        cmp     word [fz],0
        jne     .up
        test    ax,0800h                ; a tie: up only to make the last bit even
        jz      .cut
.up:    add     word [fz+2],0800h
        adc     word [fz+4],0
        adc     word [fz+6],0
        adc     word [fz+8],0
        jnc     .cut
        mov     word [fz+8],8000h       ; 1.111… + 1 ulp: 10.000…
        inc     word [fz_e]
.cut:   and     word [fz+2],0F800h
        mov     word [fz],0
        ret

; [bx] ← fz, fz_e, fz_s
fz_store:
        mov     ax,[fz+2]
        mov     [bx],ax
        mov     ax,[fz+4]
        mov     [bx+2],ax
        mov     ax,[fz+6]
        mov     [bx+4],ax
        mov     ax,[fz+8]
        mov     [bx+6],ax
        mov     ax,[fz_e]
        mov     [bx+F_E],ax
        mov     al,[fz_s]
        mov     [bx+F_S],al
        mov     byte [bx+F_C],FC_NORMAL
        ret
