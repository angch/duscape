; diskonaut for MS-DOS: the treemap viewer as a 16-bit real-mode program.
;
; Assembled by FASM (the DOS build of flat assembler 1.73) into an MZ executable, see BUILD.BAT
; and `make dos`. It needs a 386 with a 387, which is what DOSBox-X emulates by default.
;
; The Rust workspace is the reference, not a dependency:
;   the scan               scanners/src/lib.rs (one walk, sizes charged to every ancestor)
;   files_in_folder        common/src/tiles/files_in_folder.rs (largest first, zoom drops the head)
;   the squarify layout    common/src/tiles/treemap.rs (HEIGHT_WIDTH_RATIO 2.5, minimum 8x3)
;   RectFloat::round       common/src/tiles/rect_float.rs
;   navigation and zoom    common/src/tiles/board.rs, tile.rs
;   tile text and colours  viewers/tui/src/ui/grid/draw_rect.rs, rectangle_grid.rs
;   sizes                  common/src/format/display_size.rs, display_count.rs, truncate.rs
;
; Memory: every entry is a node in a bump-allocated arena of conventional memory, addressed by
; its segment (a node is 32 bytes of header then its name, rounded up to paragraphs). When the
; arena is full, what is left of a folder is charged to one "not in memory" entry in it, so the
; totals stay right even where the listing cannot be complete. That entry comes from a reserve
; kept back for it; if the reserve runs out too, the size goes to the folder itself.
;
; Long file names are used when DOS offers them (INT 21h AX=71xxh, DOSBox-X with lfn=true),
; else 8.3 names.

format MZ
entry cseg:start
stack 8000h
heap 0

MAXDEPTH        = 40            ; folders deeper than this are counted, not entered
PATHMAX         = 250
MAXENT          = 32000         ; entries of one folder that are listed
MAXTILES        = 160
RESERVE         = 800h          ; paragraphs kept for "not in memory" entries
FRAME           = 340           ; a walk's stack frame: locals and a find buffer
FD              = FRAME         ; the find buffer, at [bp-FD]

; a node, at offset 0 of its own segment
N_SIZE          = 0             ; qword, apparent size
N_DISK          = 8             ; qword, size on disk (rounded up to clusters)
N_DESC          = 16            ; dword, descendants
N_PARENT        = 20
N_CHILD         = 22            ; first child
N_NEXT          = 24            ; next sibling
N_COUNT         = 26            ; children
N_FLAGS         = 28
N_NLEN          = 29
N_NAME          = 32

F_DIR           = 1
F_PSEUDO        = 2             ; "not in memory": what the arena had no room to list
F_SHORT         = 4             ; DOS could not convert the long name: paths use the short one,
                                ; kept after the long name's NUL
F_SKIPPED       = 8             ; a folder too deep, or too long a path, to walk into
NP_MAX          = 296           ; the longest path node_path writes (its buffers hold 300)

; a tile
T_X             = 0
T_Y             = 2
T_W             = 4
T_H             = 6
T_NODE          = 8
T_IDX           = 10            ; index into the listing
T_SIZE          = 16

MINIMUM_HEIGHT  = 3
MINIMUM_WIDTH   = 8
SMALL_W         = 4             ; SMALL_FILES_MINIMUM_WIDTH
SMALL_H         = 3

BOARD_ROWS      = 23            ; rows 1..22 hold the board and its borders

; walk frame locals
S_TARGET        = 2
S_SAVED         = 4
S_HANDLE        = 6
S_PSEUDO        = 8             ; rm_tree: the first error
S_BASE          = 10
S_CHILD         = 12
S_PEND          = 14            ; where this frame's queued subfolders start
S_END           = 16            ; and end
S_CUR           = 18
PENDMAX         = 8000h         ; bytes of queued subfolders, all frames together
RM_MAXDEPTH     = 80            ; folders deleted within folders; the stack holds about 90

; box-drawing directions: which way a line leaves the cell
B_UP            = 1
B_DOWN          = 2
B_LEFT          = 4
B_RIGHT         = 8

A_TITLE         = 0Eh
A_PATH          = 0Fh
A_TEXT          = 07h
A_KEY           = 0Eh
A_DIR           = 09h
A_BORDER        = 07h
A_DIALOG        = 4Fh

segment cseg use16

start:
        mov     ax,dseg
        mov     ds,ax
        mov     [psp],es
        push    ds
        pop     es
        cld
        call    check_cpu
        fninit
        fldcw   [cw_near]
        mov     di,boxbits              ; box character → the directions it joins
        xor     ax,ax
        mov     cx,128
        rep     stosw
        mov     bx,1
@@:     mov     al,[boxch+bx]
        movzx   si,al
        mov     [boxbits+si],bl
        inc     bx
        cmp     bx,16
        jb      @b
        call    set_handlers
        call    detect_lfn
        call    parse_args
        call    resolve_root
        call    alloc_memory
        call    make_root
        call    video_init
        mov     [scanning],1
        call    ticks
        mov     [last_tick],eax
        mov     ax,[root]
        mov     [cur],ax
        call    scan_dir
        mov     [scanning],0
        cmp     [abort],0
        jne     quit
        mov     ax,[root]
        mov     [cur],ax
        mov     [zoom],0
        mov     [sel],-1
        call    change_files

main_loop:
        call    render
.wait:
        cmp     [abort],0               ; Ctrl-C
        jne     quit
        call    key_ready
        jnz     .key
        cmp     [snap],0
        je      @f
        call    write_snap
        jmp     quit
@@:     call    mouse_poll
        jc      main_loop
        hlt
        jmp     .wait
.key:
        call    read_key
        call    handle_key
        jc      quit
        jmp     main_loop

quit:
        call    video_done
        mov     ax,4C00h
        int     21h

; dx = '$'-terminated message
fatal:
        push    dx
        cmp     [video],0
        je      @f
        call    video_done
@@:     pop     dx
        mov     ah,9
        int     21h
        mov     ax,4C01h
        int     21h

nomem:
        mov     dx,msg_nomem
        jmp     fatal

; a 386 (the 32-bit registers) and a 387 (the treemap), else a message and out
check_cpu:
        pushf                           ; put back at the end
        pushf
        pop     ax
        mov     cx,ax
        and     ax,0FFFh                ; an 8086 keeps flags 12-15 set
        push    ax
        popf
        pushf
        pop     ax
        and     ax,0F000h
        cmp     ax,0F000h
        je      .old
        or      cx,7000h                ; a 286 in real mode keeps 12-14 clear
        push    cx
        popf
        pushf
        pop     ax
        test    ax,7000h
        jz      .old
        popf
        fninit
        mov     word [tmp32],5A5Ah
        fnstsw  word [tmp32]
        cmp     byte [tmp32],0
        jne     .nofpu
        ret
.old:   popf
        mov     dx,msg_cpu
        jmp     fatal
.nofpu: mov     dx,msg_fpu
        jmp     fatal

; ---------------------------------------------------------------------------------------------
; start-up

; Ctrl-C stops the scan or quits, instead of leaving the screen as it is; a critical error (a
; drive not ready) fails the call instead of asking. DOS puts its own back when we exit.
set_handlers:
        push    ds
        push    cs
        pop     ds
        mov     dx,on_break
        mov     ax,2523h
        int     21h
        mov     dx,on_critical
        mov     ax,2524h
        int     21h
        pop     ds
        ret

on_break:
        push    ds
        push    ax
        mov     ax,dseg
        mov     ds,ax
        mov     [abort],1
        pop     ax
        pop     ds
        iret

on_critical:
        mov     al,3                    ; fail
        iret

detect_lfn:
        mov     ax,7147h                ; long current directory: fails without LFN
        xor     dl,dl
        mov     si,namebuf
        stc
        int     21h
        jc      .no
        cmp     ax,7100h
        je      .no
        mov     [lfn],1
.no:    ret

; the command tail: [-a] [-k KEYS] [-s FILE] [FOLDER]
parse_args:
        push    es
        mov     es,[psp]
        movzx   cx,byte [es:80h]
        xor     bx,bx
@@:     jcxz    @f
        mov     al,[es:81h+bx]
        mov     [args+bx],al
        inc     bx
        dec     cx
        jmp     @b
@@:     mov     byte [args+bx],0
        pop     es
        mov     si,args
.next:  call    skip_blanks
        test    al,al
        jz      .done
        cmp     al,'-'
        je      .opt
        cmp     al,'/'
        je      .opt
        mov     di,pathbuf
        call    token
        cmp     cx,PATHMAX
        jae     .usage
        mov     [pathlen],cx
        jmp     .next
.opt:   inc     si
        lodsb
        or      al,20h
        cmp     al,'a'
        je      .a
        cmp     al,'k'
        je      .k
        cmp     al,'s'
        jne     .usage
        call    skip_blanks
        test    al,al
        jz      .usage
        mov     di,snapbuf
        call    token
        mov     [snap],1
        jmp     .next
.k:     call    skip_blanks
        mov     di,keysbuf
        call    token
        mov     [keyp],keysbuf
        jmp     .next
.a:     mov     [apparent],1
        mov     [size_off],N_SIZE
        cmp     byte [si],' '
        jbe     .next
.usage:
        mov     dx,msg_usage
        jmp     fatal
.done:  ret

; si past blanks; al = the character there
skip_blanks:
@@:     mov     al,[si]
        test    al,al
        jz      @f
        cmp     al,' '
        ja      @f
        inc     si
        jmp     @b
@@:     ret

; the word at si → di, zero-terminated; cx = its length
token:
        xor     cx,cx
@@:     mov     al,[si]
        cmp     al,' '
        jbe     @f
        mov     [di],al
        inc     si
        inc     di
        inc     cx
        cmp     cx,PATHMAX
        jb      @b
@@:     mov     byte [di],0
        ret

; the next key: from -k first ("E" is Enter, "X" Esc: DOS takes < and > for itself), else the
; keyboard. ax as INT 16h gives it.
read_key:
        mov     si,[keyp]
        test    si,si
        jz      .bios
        lodsb
        test    al,al
        jz      .end
        mov     [keyp],si
        xor     ah,ah
        cmp     al,'E'
        jne     @f
        mov     ax,1C0Dh
@@:     cmp     al,'X'
        jne     @f
        mov     ax,011Bh
@@:     ret
.end:   mov     [keyp],0
        cmp     [snap],0                ; the script is done: -s shows where it got to
        je      .bios
        call    write_snap
        jmp     quit
.bios:  xor     ah,ah
        int     16h
        ret

; ZF=0 if a key is waiting, scripted or typed
key_ready:
        mov     si,[keyp]
        test    si,si
        jz      @f
        cmp     byte [si],0
        jne     .r
        mov     [keyp],0
@@:     mov     ah,1
        int     16h
.r:     ret

; -s: the screen as it is, 80x25 character and attribute pairs
write_snap:
        mov     ah,3Ch
        xor     cx,cx
        mov     dx,snapbuf
        int     21h
        jc      .r
        mov     bx,ax
        mov     ah,40h
        mov     cx,80*25*2
        mov     dx,scr
        int     21h
        mov     ah,40h                  ; then the tiles, for comparing with the Rust layout
        mov     cx,tiles_end-ntiles
        mov     dx,ntiles
        int     21h
        mov     ah,3Eh
        int     21h
.r:     ret

; the folder to scan, made absolute, with its cluster size
resolve_root:
        cmp     [pathlen],0
        jne     .have
        mov     word [pathbuf],'.'
        mov     [pathlen],1
.have:
        cmp     [lfn],0
        je      .short
        mov     si,pathbuf
        mov     di,namebuf
        mov     ax,7160h
        mov     cx,0002h
        stc
        int     21h
        jnc     .ok
.short:
        mov     si,pathbuf
        mov     di,namebuf
        mov     ah,60h
        int     21h
        jc      .bad
.ok:
        mov     si,namebuf
        mov     di,pathbuf
        xor     cx,cx
@@:     lodsb
        stosb
        inc     cx
        test    al,al
        jnz     @b
        dec     cx
        cmp     cx,3                    ; keep the backslash of "C:\"
        jbe     @f
        mov     bx,cx
        cmp     byte [pathbuf+bx-1],'\'
        jne     @f
        dec     cx
        mov     byte [pathbuf+bx-1],0
@@:     cmp     cx,200
        ja      .bad
        mov     [pathlen],cx
        call    probe_dir
        jc      .bad
        call    cluster_size
        ret
.bad:
        mov     dx,msg_notdir
        jmp     fatal

; CF=1 if pathbuf is not a folder
probe_dir:
        push    bp
        mov     bp,sp
        sub     sp,FRAME
        mov     bx,[pathlen]
        cmp     byte [pathbuf+bx-1],'\'
        je      @f
        mov     byte [pathbuf+bx],'\'
        inc     bx
@@:     mov     dword [pathbuf+bx],'*.*'
        mov     dx,pathbuf
        lea     di,[bp-FD]
        call    ff_first
        jc      .err
        mov     bx,ax
        call    ff_close
        clc
        jmp     .out
.err:
        cmp     ax,12h                  ; no more files: an empty root
        je      .fine
        cmp     ax,2
        je      .fine
        stc
        jmp     .out
.fine:  clc
.out:
        pushf
        mov     bx,[pathlen]
        mov     byte [pathbuf+bx],0
        popf
        mov     sp,bp
        pop     bp
        ret

cluster_size:
        mov     dl,[pathbuf]
        and     dl,0DFh
        sub     dl,'A'-1
        mov     ah,36h
        int     21h
        cmp     ax,0FFFFh
        je      .r
        mul     cx
        test    ax,ax
        jnz     @f
        test    dx,dx
        jz      .r
@@:     mov     word [clus],ax
        mov     word [clus+2],dx
.r:     ret

; the listing's segment, then all the rest of conventional memory for the nodes
alloc_memory:
        mov     ah,48h
        mov     bx,1000h
        int     21h
        jc      nomem
        mov     [entseg],ax
        mov     ah,48h
        mov     bx,PENDMAX/16
        int     21h
        jc      nomem
        mov     [pendseg],ax
        mov     ah,48h
        mov     bx,0FFFFh
        int     21h
        cmp     bx,RESERVE+100h
        jb      nomem
        mov     ah,48h
        int     21h
        jc      nomem
        mov     [arena_next],ax
        add     ax,bx
        mov     [arena_end],ax
        sub     ax,RESERVE
        mov     [arena_limit],ax
        ret

make_root:
        mov     cx,[pathlen]
        mov     al,1
        call    alloc_node
        jc      nomem
        mov     [root],ax
        mov     es,ax
        mov     byte [es:N_FLAGS],F_DIR
        mov     si,pathbuf
        mov     cx,[pathlen]
        call    set_name
        push    ds
        pop     es
        ret

video_init:
        mov     ax,0003h
        int     10h
        mov     ax,1003h                ; bright backgrounds instead of blinking
        xor     bx,bx
        int     10h
        mov     ah,01h                  ; no text cursor
        mov     cx,2000h
        int     10h
        mov     [video],1
        push    es
        xor     ax,ax
        mov     es,ax
        mov     ax,[es:33h*4]
        or      ax,[es:33h*4+2]
        pop     es
        jz      .r
        xor     ax,ax
        int     33h
        cmp     ax,0FFFFh
        jne     .r
        mov     [mouse],1
        mov     ax,1
        int     33h
.r:     ret

video_done:
        cmp     [mouse],0
        je      @f
        mov     ax,2
        int     33h
@@:     mov     ax,0003h
        int     10h
        mov     [video],0
        ret

; ---------------------------------------------------------------------------------------------
; the tree

; cx = name length, al = 1 to dip into the reserve. ax = the node's segment, CF=1 if full.
alloc_node:
        push    bx
        push    dx
        push    cx
        push    di
        mov     bx,cx
        add     bx,N_NAME+1+15
        shr     bx,4
        mov     dx,[arena_limit]
        test    al,al
        jz      @f
        mov     dx,[arena_end]
@@:     mov     ax,[arena_next]
        add     bx,ax
        jc      .full
        cmp     bx,dx
        ja      .full
        mov     [arena_next],bx
        push    es
        push    ax
        mov     es,ax
        xor     di,di
        xor     ax,ax
        mov     cx,N_NAME/2
        rep     stosw
        pop     ax
        pop     es
        clc
        jmp     .out
.full:
        xor     ax,ax
        stc
.out:
        pop     di
        pop     cx
        pop     dx
        pop     bx
        ret

; es = node, ds:si = name, cx = length
set_name:
        push    di
        mov     [es:N_NLEN],cl
        mov     di,N_NAME
        rep     movsb
        mov     byte [es:di],0
        pop     di
        ret

; add add_app, add_disk and add_desc to node ax and every folder above it
add_up:
        push    ax
        push    fs
        push    ebx
.l:     test    ax,ax
        jz      .d
        mov     fs,ax
        mov     ebx,dword [add_app]
        add     [fs:N_SIZE],ebx
        mov     ebx,dword [add_app+4]
        adc     [fs:N_SIZE+4],ebx
        mov     ebx,dword [add_disk]
        add     [fs:N_DISK],ebx
        mov     ebx,dword [add_disk+4]
        adc     [fs:N_DISK+4],ebx
        mov     ebx,[add_desc]
        add     [fs:N_DESC],ebx
        mov     ax,[fs:N_PARENT]
        jmp     .l
.d:     pop     ebx
        pop     fs
        pop     ax
        ret

; bx = folder. ax = a new "not in memory" entry in it, or 0.
make_pseudo:
        push    bx
        mov     cx,pseudo_len
        mov     al,1
        call    alloc_node
        pop     bx
        jc      .fail
        push    es
        mov     es,ax
        mov     si,pseudo_name
        mov     cx,pseudo_len
        call    set_name
        mov     byte [es:N_FLAGS],F_PSEUDO
        mov     [es:N_PARENT],bx
        mov     fs,bx
        mov     dx,[fs:N_CHILD]
        mov     [es:N_NEXT],dx
        mov     [fs:N_CHILD],ax
        inc     word [fs:N_COUNT]
        pop     es
        ret
.fail:
        xor     ax,ax
        ret

; ---------------------------------------------------------------------------------------------
; finding files: long names when there are, else 8.3. The buffer is at ss:di.

; ds:dx = pattern. ax = handle, CF=1 on error (ax = error)
ff_first:
        cmp     [lfn],0
        je      .s
        push    es
        push    ss
        pop     es
        mov     ax,714Eh
        mov     cx,0016h                ; hidden, system, folders
        mov     si,1
        stc
        int     21h
        mov     [ent_cx],cx             ; bit 0: the long name did not convert
        pop     es
        ret
.s:     push    dx
        call    set_dta
        pop     dx
        mov     ah,4Eh
        mov     cx,16h
        int     21h
        ret

; bx = handle
ff_next:
        cmp     [lfn],0
        je      .s
        push    es
        push    ss
        pop     es
        mov     ax,714Fh
        mov     si,1
        stc
        int     21h
        mov     [ent_cx],cx
        pop     es
        ret
.s:     call    set_dta                 ; a deeper walk moved it
        mov     ah,4Fh
        int     21h
        ret

ff_close:
        cmp     [lfn],0
        je      .r
        mov     ax,71A1h
        int     21h
.r:     ret

set_dta:
        push    ds
        push    ss
        pop     ds
        mov     dx,di
        mov     ah,1Ah
        int     21h
        pop     ds
        ret

; the entry at ss:di → ent_attr, ent_size, namebuf, namelen
extract:
        mov     [ent_lossy],0
        cmp     [lfn],0
        je      .s
        test    byte [ent_cx],1         ; a name DOS could not convert: its short one too
        jz      @f
        cmp     byte [ss:di+130h],0
        je      @f
        xor     bx,bx
.sh:    mov     al,[ss:di+130h+bx]
        mov     [shortbuf+bx],al
        inc     bx
        test    al,al
        jz      .shd
        cmp     bx,13
        jb      .sh
        mov     byte [shortbuf+bx],0
        inc     bx
.shd:   dec     bx
        mov     [shortlen],bx
        mov     [ent_lossy],1
@@:     mov     al,[ss:di]
        mov     [ent_attr],al
        mov     eax,[ss:di+20h]
        mov     dword [ent_size],eax
        mov     eax,[ss:di+1Ch]
        mov     dword [ent_size+4],eax
        lea     si,[di+2Ch]
        cmp     byte [ss:si],0
        jne     .name
        lea     si,[di+130h]            ; no long name: the short one
        jmp     .name
.s:     mov     al,[ss:di+15h]
        mov     [ent_attr],al
        mov     eax,[ss:di+1Ah]
        mov     dword [ent_size],eax
        mov     dword [ent_size+4],0
        lea     si,[di+1Eh]
.name:
        xor     bx,bx
@@:     mov     al,[ss:si+bx]
        mov     [namebuf+bx],al
        test    al,al
        jz      @f
        inc     bx
        cmp     bx,255
        jb      @b
        mov     byte [namebuf+bx],0
@@:     mov     [namelen],bx
        ret

; si, cx ← the entry's name on disk: the short one when the long one did not convert
disk_name:
        mov     si,namebuf
        mov     cx,[namelen]
        cmp     [ent_lossy],0
        je      @f
        mov     si,shortbuf
        mov     cx,[shortlen]
@@:     ret

; ZF=1 if namebuf is "." or ".."
is_dots:
        cmp     byte [namebuf],'.'
        jne     .r
        cmp     [namelen],1
        je      .r
        cmp     [namelen],2
        jne     .r
        cmp     byte [namebuf+1],'.'
.r:     ret

; ---------------------------------------------------------------------------------------------
; the scan

; ax = the node that pathbuf names. Walks it, adding every entry below it to the tree.
scan_dir:
        push    bp
        mov     bp,sp
        sub     sp,FRAME
        mov     [bp-S_TARGET],ax
        mov     word [bp-S_PSEUDO],0
        mov     ax,[pend_top]
        mov     [bp-S_PEND],ax
        mov     bx,[pathlen]
        mov     [bp-S_SAVED],bx
        inc     [depth]
        cmp     byte [pathbuf+bx-1],'\'
        je      @f
        mov     byte [pathbuf+bx],'\'
        inc     bx
@@:     mov     [bp-S_BASE],bx
        mov     dword [pathbuf+bx],'*.*'
        mov     dx,pathbuf
        lea     di,[bp-FD]
        call    ff_first
        jc      .done
        mov     [bp-S_HANDLE],ax
.entry:
        lea     di,[bp-FD]
        call    extract
        call    is_dots
        je      .next
        test    [ent_attr],8            ; a volume label
        jnz     .next
        ; its path, when there is room to walk into it
        mov     [recurse_ok],0
        mov     bx,[bp-S_BASE]
        call    disk_name
        mov     ax,bx
        add     ax,cx
        cmp     ax,PATHMAX
        jae     .sizes
        mov     [pathlen],ax
        lea     di,[pathbuf+bx]
        inc     cx
        rep     movsb
        cmp     [depth],MAXDEPTH
        jae     .sizes
        mov     [recurse_ok],1
.sizes:
        xor     eax,eax
        mov     dword [add_app],eax
        mov     dword [add_app+4],eax
        mov     dword [add_disk],eax
        mov     dword [add_disk+4],eax
        mov     [add_desc],1
        test    [ent_attr],10h
        jnz     .sized
        mov     eax,dword [ent_size]
        mov     edx,dword [ent_size+4]
        mov     dword [add_app],eax
        mov     dword [add_app+4],edx
        test    edx,edx
        jnz     .disk                   ; 4 GiB and more: as it is
        mov     ecx,[clus]
        lea     ebx,[ecx-1]
        add     eax,ebx
        adc     edx,0
        div     ecx
        mul     ecx
.disk:  mov     dword [add_disk],eax
        mov     dword [add_disk+4],edx
.sized:
        mov     fs,[bp-S_TARGET]
        test    byte [fs:N_FLAGS],F_PSEUDO
        jnz     .into_target
        mov     cx,[namelen]
        cmp     [ent_lossy],0
        je      @f
        add     cx,[shortlen]
        inc     cx
@@:     xor     al,al
        call    alloc_node
        jnc     .child
        mov     ax,[bp-S_PSEUDO]        ; out of memory: charge the folder's "not in memory"
        test    ax,ax
        jnz     .rolled
        mov     bx,[bp-S_TARGET]
        call    make_pseudo
        mov     [bp-S_PSEUDO],ax
        test    ax,ax
        jnz     .rolled
.into_target:
        mov     ax,[bp-S_TARGET]
.rolled:
        call    add_up
        test    [ent_attr],10h
        jz      .next
        cmp     [recurse_ok],0
        je      .next
        call    push_pending
        jnc     .next
        call    scan_dir                ; no room to queue it: now, with the search open
        jmp     .next
.child:
        mov     [bp-S_CHILD],ax
        mov     es,ax
        mov     si,namebuf
        mov     cx,[namelen]
        call    set_name
        cmp     [ent_lossy],0
        je      @f
        or      byte [es:N_FLAGS],F_SHORT
        mov     di,[namelen]
        add     di,N_NAME+1
        mov     si,shortbuf
        mov     cx,[shortlen]
        inc     cx
        rep     movsb
@@:     mov     bx,[bp-S_TARGET]
        mov     [es:N_PARENT],bx
        mov     fs,bx
        mov     dx,[fs:N_CHILD]
        mov     [es:N_NEXT],dx
        mov     [fs:N_CHILD],ax
        inc     word [fs:N_COUNT]
        mov     eax,dword [add_app]
        mov     [es:N_SIZE],eax
        mov     eax,dword [add_app+4]
        mov     [es:N_SIZE+4],eax
        mov     eax,dword [add_disk]
        mov     [es:N_DISK],eax
        mov     eax,dword [add_disk+4]
        mov     [es:N_DISK+4],eax
        test    [ent_attr],10h
        jz      @f
        or      byte [es:N_FLAGS],F_DIR
        cmp     [recurse_ok],0
        jne     @f
        or      byte [es:N_FLAGS],F_SKIPPED
@@:     push    ds
        pop     es
        mov     ax,bx
        call    add_up
        test    [ent_attr],10h
        jz      .next
        cmp     [recurse_ok],0
        je      .next
        mov     ax,[bp-S_CHILD]
        call    push_pending
        jnc     .next
        call    scan_dir
.next:
        call    progress
        cmp     [abort],0
        jne     .close
        mov     bx,[bp-S_HANDLE]
        lea     di,[bp-FD]
        call    ff_next
        jnc     .entry
.close:
        mov     bx,[bp-S_HANDLE]
        call    ff_close
        ; then the subfolders, so that only one search is ever open: DOSBox-X loses entries
        ; of long-name searches when several are
        mov     ax,[pend_top]
        mov     [bp-S_END],ax
        mov     si,[bp-S_PEND]
.sub:   cmp     si,[bp-S_END]
        jae     .subs_done
        cmp     [abort],0
        jne     .subs_done
        call    pop_pending
        mov     [bp-S_CUR],si
        call    scan_dir
        mov     si,[bp-S_CUR]
        jmp     .sub
.subs_done:
        mov     ax,[bp-S_PEND]
        mov     [pend_top],ax
.done:
        mov     bx,[bp-S_SAVED]
        mov     [pathlen],bx
        mov     byte [pathbuf+bx],0
        dec     [depth]
        mov     sp,bp
        pop     bp
        ret

; queue the entry's disk name (disk_name), to be walked into node ax. CF=1 if there is no room.
push_pending:
        push    es
        push    di
        push    si
        push    cx
        push    ax
        call    disk_name
        pop     ax
        push    si
        mov     di,[pend_top]
        mov     si,di
        add     si,cx
        add     si,3
        cmp     si,PENDMAX
        ja      .full
        mov     es,[pendseg]
        stosw
        mov     al,cl
        stosb
        pop     si
        rep     movsb
        mov     [pend_top],di
        clc
        jmp     .out
.full:  pop     si
        stc
.out:   pop     cx
        pop     si
        pop     di
        pop     es
        ret

; the queued subfolder at si → pathbuf (after the frame's base, [bp-S_BASE]) and ax, its node;
; si past it
pop_pending:
        push    fs
        mov     fs,[pendseg]
        mov     ax,[fs:si]
        movzx   cx,byte [fs:si+2]
        add     si,3
        mov     bx,[bp-S_BASE]
        lea     di,[pathbuf+bx]
        add     bx,cx
        mov     [pathlen],bx
        jcxz    .z
@@:     mov     dl,[fs:si]
        mov     [di],dl
        inc     si
        inc     di
        loop    @b
.z:     mov     byte [di],0
        pop     fs
        ret

; now and then: the keyboard (q or Esc stops), and the treemap of what is found so far
progress:
        inc     [seen]
        cmp     [scanning],0            ; a rescan keeps the screen as it is
        je      .r
        test    byte [seen],31
        jnz     .r
        mov     ah,1
        int     16h
        jz      .t
        xor     ah,ah
        int     16h
        cmp     al,'q'
        je      .abort
        cmp     al,'Q'
        je      .abort
        cmp     al,27
        je      .abort
.t:     call    ticks
        mov     edx,eax
        sub     edx,[last_tick]
        cmp     edx,4                   ; about 0.2 s
        jb      .r
        mov     [last_tick],eax
        call    live_render
.r:     ret
.abort:
        mov     [abort],1
        ret

ticks:
        push    es
        push    40h
        pop     es
        mov     eax,[es:6Ch]
        pop     es
        ret

live_render:
        pushad
        push    es
        push    fs
        push    gs
        push    ds
        pop     es
        inc     [spin]
        mov     ax,[root]
        mov     [cur],ax
        mov     [zoom],0
        mov     [sel],-1
        call    change_files
        call    render
        pop     gs
        pop     fs
        pop     es
        popad
        ret

; ---------------------------------------------------------------------------------------------
; the listing: the folder's entries, largest first (files_in_folder)

build_listing:
        mov     gs,[entseg]
        mov     fs,[cur]
        mov     ax,[fs:N_CHILD]
        xor     bx,bx
.l:     test    ax,ax
        jz      .e
        cmp     bx,MAXENT*2
        jae     .e
        mov     [gs:bx],ax
        add     bx,2
        mov     fs,ax
        mov     ax,[fs:N_NEXT]
        jmp     .l
.e:     shr     bx,1
        mov     [nent],bx
        ; Shell sort
        mov     si,gaps
.gap:   lodsw
        test    ax,ax
        jz      .done
        mov     [gap],ax
        cmp     ax,[nent]
        jae     .gap
        mov     cx,ax
.i:     cmp     cx,[nent]
        jae     .gap
        mov     bx,cx
        shl     bx,1
        mov     ax,[gs:bx]
        mov     [tmp_e],ax
        mov     di,cx
.j:     cmp     di,[gap]
        jb      .put
        mov     bx,di
        sub     bx,[gap]
        shl     bx,1
        mov     dx,[gs:bx]
        mov     ax,[tmp_e]
        call    before
        jnc     .put
        mov     bx,di
        shl     bx,1
        mov     [gs:bx],dx
        sub     di,[gap]
        jmp     .j
.put:   mov     bx,di
        shl     bx,1
        mov     ax,[tmp_e]
        mov     [gs:bx],ax
        inc     cx
        jmp     .i
.done:  ret

; CF=1 if node ax comes before node dx: larger first, then by name
before:
        push    si
        push    fs
        push    es
        push    eax
        mov     fs,ax
        mov     es,dx
        mov     si,[size_off]
        mov     eax,[fs:si+4]
        cmp     eax,[es:si+4]
        jne     .size
        mov     eax,[fs:si]
        cmp     eax,[es:si]
        jne     .size
        mov     si,N_NAME
.n:     mov     al,[fs:si]
        cmp     al,[es:si]
        jne     .out                    ; CF=1 if a's byte is lower
        test    al,al
        jz      .no
        inc     si
        jmp     .n
.size:  ja      .yes
.no:    clc
        jmp     .out
.yes:   stc
.out:   pop     eax
        pop     es
        pop     fs
        pop     si
        ret

; ---------------------------------------------------------------------------------------------
; the treemap (treemap.rs), in doubles on the 387

change_files:
        call    build_listing
fill:
        call    tm_init
        call    setup_files
        call    squarify
        mov     ax,[sel]
        cmp     ax,-1
        je      @f
        cmp     ax,[ntiles]
        jb      @f
        mov     [sel],-1
@@:     ret

tm_init:
        fild    word [bnd_x]
        fstp    qword [em_x]
        fild    word [bnd_y]
        fstp    qword [em_y]
        fild    word [bnd_w]
        fstp    qword [em_w]
        fild    word [bnd_h]
        fstp    qword [em_h]
        fld     qword [em_w]
        fmul    qword [em_h]
        fstp    qword [tot_area]
        mov     [ntiles],0
        mov     [has_small],0
        ret

; the total the percentages are of: the folder's size, or what its entries add up to if more,
; less what the zoom leaves off
setup_files:
        mov     ax,[zoom]
        cmp     ax,[nent]
        jbe     @f
        mov     ax,[nent]
        mov     [zoom],ax
@@:     mov     cx,[nent]
        sub     cx,ax
        mov     [nfiles],cx
        fldz
        xor     bx,bx
.sum:   cmp     bx,[nent]
        jae     .summed
        call    load_size
        faddp   st1,st0
        inc     bx
        jmp     .sum
.summed:
        mov     fs,[cur]
        mov     si,[size_off]
        fild    qword [fs:si]
        fcom    st1
        fnstsw  ax
        sahf
        jae     .folder
        fstp    st0
        jmp     .total
.folder:
        fstp    st1
.total:
        xor     bx,bx
.drop:  cmp     bx,[zoom]
        jae     .dropped
        call    load_size
        fsubp   st1,st0
        inc     bx
        jmp     .drop
.dropped:
        fst     qword [ftotal]
        ftst
        fnstsw  ax
        sahf
        fstp    st0
        je      .equal
        mov     [equal],0
        ret
.equal:
        mov     [equal],1               ; nothing has a size: equal shares
        cmp     [nfiles],0
        je      @f
        fld1
        fidiv   word [nfiles]
        fstp    qword [eq_pct]
@@:     ret

; bx = listing index. st0 = its size.
load_size:
        push    fs
        push    si
        mov     si,bx
        shl     si,1
        mov     fs,[gs:si]
        mov     si,[size_off]
        fild    qword [fs:si]
        pop     si
        pop     fs
        ret

; bx = listing index. st0 = its share of the folder (1.0 is all of it).
pct_of:
        cmp     [equal],0
        jne     .eq
        call    load_size
        fdiv    qword [ftotal]
        ret
.eq:    fld     qword [eq_pct]
        ret

; bx = listing index. st0 = the area it gets on the board.
area_of:
        call    pct_of
        fmul    qword [tot_area]
        ret

squarify:
        mov     ax,[zoom]
        mov     [rs],ax
        mov     [ri],ax
.loop:
        call    compute_len
        mov     ax,[ri]
        cmp     ax,[nent]
        jb      .more
        mov     ax,[rs]
        mov     dx,[ri]
        call    layoutrow
        jmp     .done
.more:
        mov     bx,[ri]                 ; largest first, so this is the largest left
        call    area_of
        fcomp   qword [min_prod]
        fnstsw  ax
        sahf
        jae     .renderable
        mov     ax,[rs]
        mov     dx,[ri]
        call    layoutrow
        mov     ax,[ri]
        mov     dx,[nent]
        call    layoutrow
        jmp     .done
.renderable:
        mov     ax,[rs]
        mov     dx,[ri]
        call    worst
        jc      .add
        fstp    qword [w_cur]
        mov     ax,[rs]
        mov     dx,[ri]
        inc     dx
        call    worst
        jc      .lay
        fstp    qword [w_nxt]
        fld     qword [w_cur]
        fcomp   qword [w_nxt]
        fnstsw  ax
        sahf
        jb      .add
.lay:
        mov     ax,[rs]
        mov     dx,[ri]
        call    layoutrow
        mov     ax,[ri]
        mov     [rs],ax
        jmp     .loop
.add:
        inc     [ri]
        jmp     .loop
.done:
        ; the "small files" corner is a rectangle: nothing drawn inside it
        cmp     [has_small],0
        je      .r
        mov     si,tiles
        mov     di,tiles
        mov     cx,[ntiles]
        xor     dx,dx
.k:     jcxz    .kept
        mov     ax,[si+T_X]
        cmp     ax,[small_x]
        jb      .keep
        mov     ax,[si+T_Y]
        cmp     ax,[small_y]
        jb      .keep
        add     si,T_SIZE
        dec     cx
        jmp     .k
.keep:  push    cx
        mov     cx,T_SIZE/2
        rep     movsw
        pop     cx
        inc     dx
        dec     cx
        jmp     .k
.kept:  mov     [ntiles],dx
.r:     ret

compute_len:
        fld     qword [em_h]
        fmul    qword [c_ratio]
        fcom    qword [em_w]
        fnstsw  ax
        sahf
        jae     .narrow
        fstp    qword [len]
        fld     qword [c_minh]
        fmul    qword [c_ratio]
        fstp    qword [min1]
        fld     qword [c_minw]
        fdiv    qword [c_ratio]
        fstp    qword [min2]
        jmp     .p
.narrow:
        fstp    st0
        fld     qword [em_w]
        fdiv    qword [c_ratio]
        fstp    qword [len]
        fld     qword [c_minw]
        fdiv    qword [c_ratio]
        fstp    qword [min1]
        fld     qword [c_minh]
        fmul    qword [c_ratio]
        fstp    qword [min2]
.p:     fld     qword [min1]
        fmul    qword [min2]
        fstp    qword [min_prod]
        ret

; the worst aspect ratio of the row [ax, dx): st0, or CF=1 if the row is empty or has an
; entry that cannot be drawn (worst_in_renderable_row)
worst:
        cmp     ax,dx
        jae     .none
        mov     [w_a],ax
        mov     [w_b],dx
        fldz
        mov     bx,ax
.s:     call    area_of
        faddp   st1,st0
        inc     bx
        cmp     bx,[w_b]
        jb      .s
        fstp    qword [w_sum]
        mov     [w_have],0
        mov     bx,[w_a]
.l:     call    area_of
        fst     qword [w_size]
        fdiv    qword [w_sum]
        fmul    qword [len]
        fst     qword [w_first]
        fdivr   qword [w_size]
        fstp    qword [w_second]
        fld     qword [w_first]
        fcomp   qword [min1]
        fnstsw  ax
        sahf
        jb      .none
        fld     qword [w_second]
        fcomp   qword [min2]
        fnstsw  ax
        sahf
        jb      .none
        fld     qword [w_first]
        fcomp   qword [w_second]
        fnstsw  ax
        sahf
        jb      .fs
        fld     qword [w_second]
        fdiv    qword [w_first]
        jmp     .r
.fs:    fld     qword [w_first]
        fdiv    qword [w_second]
.r:     cmp     [w_have],0
        je      .take
        fcom    qword [w_worst]
        fnstsw  ax
        sahf
        jb      .take
        fstp    st0
        jmp     .nx
.take:  fstp    qword [w_worst]
        mov     [w_have],1
.nx:    inc     bx
        cmp     bx,[w_b]
        jb      .l
        fld     qword [w_worst]
        clc
        ret
.none:  stc
        ret

; lay out the row [ax, dx) along the empty space's shorter side
layoutrow:
        cmp     ax,dx
        jae     .ret
        mov     [l_a],ax
        mov     [l_b],dx
        fldz
        mov     bx,ax
.s:     call    area_of
        faddp   st1,st0
        inc     bx
        cmp     bx,[l_b]
        jb      .s
        fstp    qword [l_total]
        mov     [l_horiz],0
        fld     qword [em_h]
        fmul    qword [c_ratio]
        fcomp   qword [em_w]
        fnstsw  ax
        sahf
        jb      @f
        mov     [l_horiz],1
@@:     fld     qword [em_y]
        cmp     [l_horiz],0
        je      @f
        fstp    st0
        fld     qword [em_x]
@@:     fstp    qword [l_prog]
        fldz
        fstp    qword [l_second]
        mov     bx,[l_a]
.l:     call    area_of
        fst     qword [l_size]
        fdiv    qword [l_total]
        cmp     [l_horiz],0
        je      .v1
        fmul    qword [em_w]
        jmp     .f1
.v1:    fmul    qword [em_h]
.f1:    fst     qword [l_first]
        fdivr   qword [l_size]          ; the candidate for the other side
        fld     qword [l_second]
        fcom    st1
        fnstsw  ax
        sahf
        ja      .keep
        fstp    st0
        jmp     .gs
.keep:  fstp    st1
.gs:    fstp    qword [l_tsecond]
        cmp     [l_horiz],0
        je      .vr
        fld     qword [l_prog]
        fstp    qword [r_x]
        fld     qword [em_y]
        fstp    qword [r_y]
        fld     qword [l_first]
        fstp    qword [r_w]
        fld     qword [l_tsecond]
        fstp    qword [r_h]
        jmp     .rr
.vr:    fld     qword [em_x]
        fstp    qword [r_x]
        fld     qword [l_prog]
        fstp    qword [r_y]
        fld     qword [l_tsecond]
        fstp    qword [r_w]
        fld     qword [l_first]
        fstp    qword [r_h]
.rr:    fld     qword [l_prog]
        fadd    qword [l_first]
        fstp    qword [l_prog]
        call    round_rect
        cmp     [t_h],MINIMUM_HEIGHT
        jb      .small
        cmp     [t_w],MINIMUM_WIDTH
        jb      .small
        call    push_tile
        jmp     .upd
.small: call    add_unrenderable
.upd:   fld     qword [l_tsecond]
        fcomp   qword [l_second]
        fnstsw  ax
        sahf
        jbe     .nx
        fld     qword [l_tsecond]
        fstp    qword [l_second]
.nx:    inc     bx
        cmp     bx,[l_b]
        jb      .l
        cmp     [l_horiz],0
        je      .vs
        fld     qword [em_h]
        fsub    qword [l_second]
        fstp    qword [em_h]
        fld     qword [em_y]
        fadd    qword [l_second]
        fstp    qword [em_y]
        ret
.vs:    fld     qword [em_w]
        fsub    qword [l_second]
        fstp    qword [em_w]
        fld     qword [em_x]
        fadd    qword [l_second]
        fstp    qword [em_x]
.ret:   ret

; r_x..r_h → t_x..t_h (RectFloat::round)
round_rect:
        fld     qword [r_x]
        call    round_u16
        mov     [t_x],ax
        fld     qword [r_y]
        call    round_u16
        mov     [t_y],ax
        fld     qword [r_x]
        fisub   word [t_x]
        fadd    qword [r_w]
        call    round_u16
        mov     [t_w],ax
        fld     qword [r_y]
        fisub   word [t_y]
        fadd    qword [r_h]
        call    round_u16
        mov     [t_h],ax
        fld     qword [r_x]
        fadd    qword [r_w]
        call    round_u16
        mov     dx,[t_x]
        add     dx,[t_w]
        cmp     ax,dx
        jbe     @f
        inc     [t_w]
@@:     fld     qword [r_y]
        fadd    qword [r_h]
        call    round_u16
        mov     dx,[t_y]
        add     dx,[t_h]
        cmp     ax,dx
        jbe     @f
        inc     [t_h]
@@:     ret

; st0 (popped) → ax: rounded half away from zero, saturating like Rust's `as u16`
round_u16:
        fcom    st0
        fnstsw  ax
        sahf
        jp      .zero
        fadd    qword [c_half]
        fcom    qword [c_65535]
        fnstsw  ax
        sahf
        jae     .max
        fldcw   [cw_chop]
        fistp   dword [tmp32]
        fldcw   [cw_near]
        mov     eax,[tmp32]
        test    eax,eax
        js      .zero2
        ret
.max:   fstp    st0
        mov     ax,0FFFFh
        ret
.zero:  fstp    st0
.zero2: xor     ax,ax
        ret

; st0 (popped) → eax, rounded half to even, as Rust's {:.1} and {:.0} print (the rest of
; round_u32's rules)
round_even:
        fcom    st0
        fnstsw  ax
        sahf
        jp      round_u32.zero
        fistp   qword [tmp64]
        jmp     round_u32.got

; st0 (popped) → eax, rounded half up
round_u32:
        fcom    st0
        fnstsw  ax
        sahf
        jp      .zero
        fadd    qword [c_half]
        fldcw   [cw_chop]
        fistp   qword [tmp64]
        fldcw   [cw_near]
.got:   cmp     dword [tmp64+4],0
        jl      .zero2
        jne     .max
        mov     eax,dword [tmp64]
        ret
.max:   mov     eax,0FFFFFFFFh
        ret
.zero:  fstp    st0
.zero2: xor     eax,eax
        ret

; grow the "small files" corner to cover t_x, t_y (add_unrenderable_tile)
add_unrenderable:
        mov     ax,[bnd_x]
        add     ax,[bnd_w]
        sub     ax,SMALL_W
        jnc     @f
        xor     ax,ax
@@:     mov     dx,[t_x]
        cmp     dx,ax
        jbe     @f
        mov     dx,ax
@@:     cmp     dx,[bnd_x]
        jae     @f
        mov     dx,[bnd_x]
@@:     mov     ax,[bnd_y]
        add     ax,[bnd_h]
        sub     ax,SMALL_H
        jnc     @f
        xor     ax,ax
@@:     mov     cx,[t_y]
        cmp     cx,ax
        jbe     @f
        mov     cx,ax
@@:     cmp     cx,[bnd_y]
        jae     @f
        mov     cx,[bnd_y]
@@:     cmp     [has_small],0
        je      .first
        cmp     dx,[small_x]
        jae     @f
        mov     [small_x],dx
@@:     cmp     cx,[small_y]
        jae     @f
        mov     [small_y],cx
@@:     ret
.first: mov     [small_x],dx
        mov     [small_y],cx
        mov     [has_small],1
        ret

; bx = listing index of the tile t_x..t_h
push_tile:
        mov     ax,[ntiles]
        cmp     ax,MAXTILES
        jae     .r
        mov     di,ax
        shl     di,4
        add     di,tiles
        mov     ax,[t_x]
        mov     [di+T_X],ax
        mov     ax,[t_y]
        mov     [di+T_Y],ax
        mov     ax,[t_w]
        mov     [di+T_W],ax
        mov     ax,[t_h]
        mov     [di+T_H],ax
        mov     si,bx
        shl     si,1
        mov     ax,[gs:si]
        mov     [di+T_NODE],ax
        mov     [di+T_IDX],bx
        inc     [ntiles]
.r:     ret

; ---------------------------------------------------------------------------------------------
; drawing, into `scr`, then onto the screen at once

render:
        push    ds
        pop     es
        mov     di,scr
        mov     ax,0720h
        mov     cx,80*25
        rep     stosw
        call    draw_title
        cmp     [ntiles],0
        jne     .tiles
        call    draw_empty
.tiles:
        xor     cx,cx
.t:     cmp     cx,[ntiles]
        jae     .tdone
        mov     si,cx
        shl     si,4
        add     si,tiles
        mov     ax,[si+T_X]             ; fits(): inside the board
        add     ax,[si+T_W]
        cmp     ax,80
        jae     .tn
        mov     ax,[si+T_Y]
        add     ax,[si+T_H]
        cmp     ax,BOARD_ROWS
        jae     .tn
        push    cx
        call    draw_tile
        pop     cx
        mov     si,cx
        shl     si,4
        add     si,tiles
        mov     dl,[si+T_X]
        mov     dh,[si+T_Y]
        mov     cl,[si+T_W]
        mov     ch,[si+T_H]
        call    mask_rect
        mov     cx,si
        sub     cx,tiles
        shr     cx,4
.tn:    inc     cx
        jmp     .t
.tdone:
        call    draw_small
        call    draw_status
        call    draw_legend
blit:
        cmp     [mouse],0
        je      @f
        mov     ax,2
        int     33h
@@:     push    es
        mov     ax,0B800h
        mov     es,ax
        xor     di,di
        mov     si,scr
        mov     cx,80*25
        rep     movsw
        pop     es
        cmp     [mouse],0
        je      @f
        mov     ax,1
        int     33h
@@:     ret

; dl = x, dh = y, al = character, ah = attribute
put_ch:
        cmp     dl,80
        jae     .r
        cmp     dh,25
        jae     .r
        push    bx
        push    dx
        movzx   bx,dh
        imul    bx,bx,80
        movzx   dx,dl
        add     bx,dx
        shl     bx,1
        mov     [scr+bx],ax
        pop     dx
        pop     bx
.r:     ret

; dl = x, dh = y, ds:si = text, cx = length, ah = attribute. Clipped at the right edge.
put_str:
        jcxz    .r
.l:     cmp     dl,80
        jae     .r
        lodsb
        call    put_ch
        inc     dl
        loop    .l
.r:     ret

; dl = x, dh = y, al = direction bits; only on the board's rows. Joined with the line already
; in the cell, if any, and the cell keeps its colours (draw_next_symbol)
mask_or:
        cmp     dl,80
        jae     .r
        cmp     dh,BOARD_ROWS
        jae     .r
        push    ax
        push    bx
        push    dx
        movzx   bx,dh
        imul    bx,bx,80
        movzx   dx,dl
        add     bx,dx
        shl     bx,1
        movzx   dx,byte [scr+bx]
        xchg    bx,dx
        or      al,[boxbits+bx]
        movzx   bx,al
        mov     al,[boxch+bx]
        mov     bx,dx
        mov     byte [scr+bx],al
        pop     dx
        pop     bx
        pop     ax
.r:     ret

; the outline of dl, dh, cl = w, ch = h, its right and bottom edges at x+w and y+h
; (draw_rect_on_grid)
mask_rect:
        test    cl,cl
        jz      .r
        test    ch,ch
        jz      .r
        push    dx
        mov     al,B_DOWN+B_RIGHT
        call    mask_or
        add     dh,ch
        mov     al,B_UP+B_RIGHT
        call    mask_or
        add     dl,cl
        mov     al,B_UP+B_LEFT
        call    mask_or
        sub     dh,ch
        mov     al,B_DOWN+B_LEFT
        call    mask_or
        pop     dx
        push    dx
        mov     bl,1
.h:     cmp     bl,cl
        jae     .hd
        push    dx
        add     dl,bl
        mov     al,B_LEFT+B_RIGHT
        call    mask_or
        add     dh,ch
        call    mask_or
        pop     dx
        inc     bl
        jmp     .h
.hd:    mov     bl,1
.v:     cmp     bl,ch
        jae     .vd
        push    dx
        add     dh,bl
        mov     al,B_UP+B_DOWN
        call    mask_or
        add     dl,cl
        call    mask_or
        pop     dx
        inc     bl
        jmp     .v
.vd:    pop     dx
.r:     ret

; dl, dh, cl = w, ch = h: cells [x, x+w) × [y, y+h) set to ax
fill_cells:
        test    cl,cl
        jz      .r
        test    ch,ch
        jz      .r
        push    dx
        push    cx
.row:   push    dx
        push    cx
.col:   call    put_ch
        inc     dl
        dec     cl
        jnz     .col
        pop     cx
        pop     dx
        inc     dh
        dec     ch
        jnz     .row
        pop     cx
        pop     dx
.r:     ret

; ds:si zero-terminated → es:di
append_z:
@@:     lodsb
        test    al,al
        jz      @f
        stosb
        jmp     @b
@@:     ret

draw_title:
        mov     di,tb
        mov     si,s_total
        cmp     [scanning],0
        je      @f
        mov     si,s_scanning
@@:     call    append_z
        cmp     [apparent],0
        je      @f
        mov     si,s_apparent
        call    append_z
@@:     mov     si,s_colon
        call    append_z
        mov     fs,[root]
        mov     si,[size_off]
        fild    qword [fs:si]
        call    fmt_size
        mov     si,s_lparen
        call    append_z
        mov     eax,[fs:N_DESC]
        call    u32dec_c
        mov     si,s_files_close
        call    append_z
        cmp     [scanning],0
        jne     .spin
        mov     si,s_freed
        call    append_z
        mov     si,[size_off]
        fild    qword [freed+si]
        call    fmt_size
        jmp     .left
.spin:  mov     al,' '
        stosb
        movzx   bx,[spin]
        and     bx,3
        mov     al,[spinner+bx]
        stosb
.left:  mov     al,' '
        stosb
        mov     cx,di
        sub     cx,tb
        mov     [title_len],cx
        mov     si,tb
        xor     dx,dx
        mov     ah,A_TITLE
        call    put_str
        ; the folder shown, on the right
        mov     ax,[cur]
        mov     di,tb
        mov     al,' '
        stosb
        mov     ax,[cur]
        call    node_path
        cmp     ax,[root]
        je      .r1
        mov     fs,ax
        mov     si,s_lparen
        call    append_z
        mov     si,[size_off]
        fild    qword [fs:si]
        call    fmt_size
        mov     si,s_comma
        call    append_z
        mov     eax,[fs:N_DESC]
        call    u32dec_c
        mov     si,s_files_close
        call    append_z
.r1:    mov     al,' '
        stosb
        mov     cx,di
        sub     cx,tb
        mov     si,tb
        mov     ax,80
        sub     ax,[title_len]
        cmp     ax,8
        jb      .r
        cmp     cx,ax
        jbe     .fit
        ; too long: keep its end, after "..."
        mov     bx,cx
        sub     bx,ax
        add     bx,3
        add     si,bx
        sub     cx,bx
        push    si
        push    cx
        mov     dl,byte [title_len]
        mov     dh,0
        mov     si,s_dots
        mov     cx,3
        mov     ah,A_PATH
        call    put_str
        pop     cx
        pop     si
        mov     dl,byte [title_len]
        add     dl,3
        jmp     .put
.fit:   mov     dl,80
        sub     dl,cl
.put:   mov     dh,0
        mov     ah,A_PATH
        call    put_str
.r:     ret

draw_status:
        mov     di,tb
        mov     bx,[sel]
        cmp     bx,-1
        je      .none
        shl     bx,4
        add     bx,tiles
        mov     fs,[bx+T_NODE]
        push    word [bx+T_IDX]
        mov     si,s_selected
        call    append_z
        movzx   cx,byte [fs:N_NLEN]
        xor     si,si
@@:     jcxz    @f
        mov     al,[fs:N_NAME+si]
        stosb
        inc     si
        dec     cx
        jmp     @b
@@:     test    byte [fs:N_FLAGS],F_DIR
        jz      @f
        mov     al,'/'
        stosb
@@:     mov     si,s_sep
        call    append_z
        pop     bx
        push    bx
        call    load_size
        call    fmt_size
        mov     si,s_lparen
        call    append_z
        pop     bx
        call    pct_of
        fmul    qword [c_100]
        call    round_even
        call    u32dec
        mov     si,s_pct_close
        call    append_z
        test    byte [fs:N_FLAGS],F_DIR
        jz      .show
        mov     si,s_sep
        call    append_z
        mov     eax,[fs:N_DESC]
        call    u32dec_c
        mov     si,s_files
        call    append_z
        test    byte [fs:N_FLAGS],F_SKIPPED
        jz      .show
        mov     si,s_skipped
        call    append_z
        jmp     .show
.none:
        cmp     [scanning],0
        je      .hint
        mov     si,s_scanning_path
        call    append_z
        mov     si,pathbuf
        call    append_z
        jmp     .show
.hint:  mov     si,s_hint
        call    append_z
.show:
        mov     cx,di
        sub     cx,tb
        mov     si,tb
        mov     dl,0
        mov     dh,23
        mov     ah,A_PATH
        call    put_str
        cmp     [zoom],0
        je      .r
        mov     di,tb
        mov     si,s_zoom
        call    append_z
        movzx   eax,[zoom]
        call    u32dec
        mov     al,' '
        stosb
        mov     cx,di
        sub     cx,tb
        mov     dl,80
        sub     dl,cl
        mov     dh,23
        mov     si,tb
        mov     ah,70h
        call    put_str
.r:     ret

; the legend: '~' switches between key and text colours
draw_legend:
        mov     si,s_legend
        cmp     [apparent],0
        je      @f
        mov     si,s_legend_a
@@:     mov     dl,0
        mov     dh,24
        mov     ah,A_TEXT
.l:     lodsb
        test    al,al
        jz      .r
        cmp     al,'~'
        jne     .c
        xor     ah,A_TEXT xor A_KEY
        jmp     .l
.c:     call    put_ch
        inc     dl
        jmp     .l
.r:     ret

draw_empty:
        cmp     [scanning],0
        jne     .r
        mov     dl,32
        mov     dh,10
        mov     si,s_empty
        mov     cx,15
        mov     ah,A_TEXT
        call    put_str
        mov     dl,byte [bnd_x]
        mov     dh,byte [bnd_y]
        mov     cl,byte [bnd_w]
        mov     ch,byte [bnd_h]
        call    mask_rect
.r:     ret

draw_small:
        cmp     [has_small],0
        je      .r
        mov     dl,byte [small_x]
        mov     dh,byte [small_y]
        mov     cl,byte [bnd_x]
        add     cl,byte [bnd_w]
        sub     cl,dl
        mov     ch,byte [bnd_y]
        add     ch,byte [bnd_h]
        sub     ch,dh
        push    dx
        push    cx
        inc     dl
        inc     dh
        dec     cl
        dec     ch
        mov     ax,0F000h+'x'
        call    fill_cells
        pop     cx
        pop     dx
        call    mask_rect
.r:     ret

; si = tile (draw_tile_text_on_grid)
draw_tile:
        mov     [dt_tile],si
        mov     fs,[si+T_NODE]
        mov     cx,[si+T_W]
        sub     cx,2
        mov     [maxlen],cx
        call    build_line1
        mov     si,[dt_tile]
        mov     bx,[si+T_IDX]
        call    build_line2
        ; styles (tile_style)
        mov     si,[dt_tile]
        mov     ax,si
        sub     ax,tiles
        shr     ax,4
        mov     [a1],A_TEXT
        mov     [a2],A_TEXT
        test    byte [fs:N_FLAGS],F_DIR
        jz      @f
        mov     [a1],A_DIR
@@:     cmp     ax,[sel]
        jne     .text
        mov     [a1],70h
        mov     [a2],70h
        mov     bx,7700h+0DBh
        test    byte [fs:N_FLAGS],F_DIR
        jz      @f
        mov     [a1],1Fh
        mov     [a2],10h
        mov     bx,1100h+0DBh
@@:     mov     dl,[si+T_X]
        mov     dh,[si+T_Y]
        mov     cl,[si+T_W]
        mov     ch,[si+T_H]
        inc     dl
        inc     dh
        dec     cl
        dec     ch
        mov     ax,bx
        call    fill_cells
.text:
        mov     si,[dt_tile]
        ; rows
        mov     ax,[si+T_H]
        mov     bx,[si+T_Y]
        cmp     ax,5
        ja      .tall
        je      .five
        cmp     ax,4
        je      .four
        lea     cx,[bx+1]
        lea     dx,[bx+2]
        jmp     .rows
.four:  lea     cx,[bx+1]
        lea     dx,[bx+3]
        jmp     .rows
.five:  lea     cx,[bx+2]
        lea     dx,[bx+3]
        jmp     .rows
.tall:  mov     cx,ax
        shr     cx,1
        add     cx,bx
        mov     dx,cx
        dec     cx
        inc     dx
        test    al,1
        jz      .rows
        inc     dx
.rows:  mov     [r1],cl
        mov     [r2],dl
        ; the first line, centred: ceil((w - len) / 2)
        mov     ax,[si+T_W]
        sub     ax,[l1len]
        inc     ax
        shr     ax,1
        add     ax,[si+T_X]
        mov     dl,al
        mov     dh,[r1]
        mov     cx,[l1len]
        push    si
        mov     si,l1buf
        mov     ah,[a1]
        call    put_str
        pop     si
        mov     ax,[si+T_W]
        sub     ax,[l2len]
        inc     ax
        shr     ax,1
        add     ax,[si+T_X]
        mov     dl,al
        mov     dh,[r2]
        mov     cx,[l2len]
        mov     si,l2buf
        mov     ah,[a2]
        call    put_str
        ret

; fs = node → l1buf, l1len: its name, a folder's with "/" and its descendants if they fit
build_line1:
        mov     di,l1buf
        movzx   cx,byte [fs:N_NLEN]
        xor     si,si
@@:     jcxz    @f
        mov     al,[fs:N_NAME+si]
        stosb
        inc     si
        dec     cx
        jmp     @b
@@:     test    byte [fs:N_FLAGS],F_DIR
        jz      .trunc
        mov     al,'/'
        stosb
        mov     ax,di
        sub     ax,l1buf
        mov     [fnlen],ax
        push    di
        mov     di,indbuf
        mov     al,'('
        stosb
        mov     al,'+'
        stosb
        mov     eax,[fs:N_DESC]
        call    u32dec_c
        mov     [ind_short],di
        mov     si,s_descendants
        call    append_z
        mov     cx,di
        sub     cx,indbuf
        pop     di
        mov     ax,[fnlen]
        add     ax,cx
        cmp     ax,[maxlen]
        jbe     .with
        mov     cx,[ind_short]
        sub     cx,indbuf
        inc     cx
        mov     ax,[fnlen]
        add     ax,cx
        cmp     ax,[maxlen]
        ja      .trunc
        mov     byte [di],' '
        inc     di
        dec     cx
        mov     si,indbuf
        rep     movsb
        mov     al,')'
        stosb
        jmp     .done
.with:  mov     byte [di],' '
        inc     di
        mov     si,indbuf
        rep     movsb
.done:  mov     ax,di
        sub     ax,l1buf
        mov     [l1len],ax
        ret
.trunc: mov     cx,di
        sub     cx,l1buf
        mov     si,l1buf
        mov     dx,[maxlen]
        call    truncate_middle
        mov     [l1len],cx
        ret

; si = text, cx = length, dx = the most it may take; in place, cx = the new length
truncate_middle:
        cmp     cx,dx
        jbe     .r
        cmp     dx,6
        jae     .mid
        mov     cx,dx
.r:     ret
.mid:   push    si
        mov     bx,dx
        shr     bx,1
        sub     bx,2                    ; split
        mov     di,trbuf
        push    cx
        mov     cx,bx
        rep     movsb                   ; the head
        pop     cx
        push    si
        mov     si,s_trunc_even
        test    dl,1
        jz      @f
        mov     si,s_trunc_odd
@@:     call    append_z
        pop     si
        pop     ax                      ; the text
        push    ax
        mov     si,ax
        add     si,cx
        sub     si,bx                   ; the tail
        mov     cx,bx
        rep     movsb
        mov     cx,di
        sub     cx,trbuf
        pop     di
        push    cx
        mov     si,trbuf
        rep     movsb
        pop     cx
        ret

; bx = listing index, maxlen = room → l2buf, l2len: the size and share (tile_second_line)
build_line2:
        push    bx
        call    pct_of
        fmul    qword [c_100]
        fld     st0
        call    round_even              ; "{:.0}%"
        mov     [pctv_even],eax
        call    round_u32               ; "{}%" of .round()
        mov     [pctv],eax
        pop     bx
        push    bx
        call    load_size
        mov     di,s1buf
        call    fmt_size
        mov     ax,di
        sub     ax,s1buf
        mov     [s1len],ax
        pop     bx
        call    load_size
        mov     di,s2buf
        call    fmt_size_r
        mov     ax,di
        sub     ax,s2buf
        mov     [s2len],ax
        mov     di,l2buf
        mov     ax,[s1len]
        add     ax,7
        cmp     [maxlen],ax
        jb      .b
        mov     si,s1buf
        mov     cx,[s1len]
        rep     movsb
        mov     si,s_lparen
        call    append_z
        mov     eax,[pctv_even]
        call    u32dec
        mov     si,s_pct_close
        call    append_z
        jmp     .done
.b:     mov     ax,[s1len]
        cmp     [maxlen],ax
        jbe     .c
        mov     si,s1buf
        mov     cx,ax
        rep     movsb
        jmp     .done
.c:     mov     ax,[s2len]
        cmp     [maxlen],ax
        jbe     .d
        mov     si,s2buf
        mov     cx,ax
        rep     movsb
        jmp     .done
.d:     cmp     [maxlen],6
        jbe     .e
        mov     al,'('
        stosb
        mov     eax,[pctv]
        call    u32dec
        mov     si,s_pct_close
        call    append_z
        jmp     .done
.e:     mov     eax,[pctv]
        call    u32dec
        mov     al,'%'
        stosb
.done:  mov     ax,di
        sub     ax,l2buf
        mov     [l2len],ax
        ret

; ---------------------------------------------------------------------------------------------
; numbers

; eax → es:di in decimal
u32dec:
        push    eax
        push    ebx
        push    ecx
        push    edx
        mov     ebx,10
        xor     cx,cx
.l:     xor     edx,edx
        div     ebx
        push    dx
        inc     cx
        test    eax,eax
        jnz     .l
.o:     pop     ax
        add     al,'0'
        stosb
        loop    .o
        pop     edx
        pop     ecx
        pop     ebx
        pop     eax
        ret

; eax → es:di with thousands separated (DisplayCount)
u32dec_c:
        push    eax
        push    ebx
        push    ecx
        push    edx
        mov     ebx,10
        xor     cx,cx
.l:     xor     edx,edx
        div     ebx
        push    dx
        inc     cx
        test    eax,eax
        jnz     .l
.o:     pop     ax
        add     al,'0'
        stosb
        dec     cx
        jz      .d
        mov     ax,cx
        mov     bl,3
        div     bl
        test    ah,ah
        jnz     .o
        mov     al,','
        stosb
        jmp     .o
.d:     pop     edx
        pop     ecx
        pop     ebx
        pop     eax
        ret

; st0 (popped) bytes → es:di, e.g. "5.2G" (DisplaySize)
fmt_size:
        push    bx
        call    size_unit
        jc      .plain
        fmul    qword [c_ten]
        call    round_even
        push    edx
        push    ecx
        xor     edx,edx
        mov     ecx,10
        div     ecx
        call    u32dec
        mov     al,'.'
        stosb
        mov     al,dl
        add     al,'0'
        stosb
        pop     ecx
        pop     edx
        mov     al,bl
        stosb
        pop     bx
        ret
.plain: call    round_u32
        call    u32dec
        pop     bx
        ret

; the same without the decimal, e.g. "5G" (DisplaySizeRounded)
fmt_size_r:
        push    bx
        call    size_unit
        jc      .plain
        call    round_even
        call    u32dec
        mov     al,bl
        stosb
        pop     bx
        ret
.plain: call    round_u32
        call    u32dec
        pop     bx
        ret

; st0 bytes → st0 in its unit and bl the unit's letter, or CF=1 for plain bytes
size_unit:
        fcom    qword [c_g]
        fnstsw  ax
        sahf
        ja      .g
        fcom    qword [c_m]
        fnstsw  ax
        sahf
        ja      .m
        fcom    qword [c_k]
        fnstsw  ax
        sahf
        ja      .k
        stc
        ret
.g:     fdiv    qword [c_gib]
        mov     bl,'G'
        clc
        ret
.m:     fdiv    qword [c_mib]
        mov     bl,'M'
        clc
        ret
.k:     fdiv    qword [c_kib]
        mov     bl,'K'
        clc
        ret

; ax = node → es:di its full path; ax unchanged. With [np_disk] set, as DOS must be given it:
; short names where the long ones did not convert (np_disk is cleared again). CF=1 if it is
; longer than NP_MAX, and then cut there.
node_path:
        push    ax
        push    bx
        push    cx
        push    si
        push    fs
        mov     [np_start],di
        mov     [np_cut],0
        xor     bx,bx
.up:    mov     [chain+bx],ax
        add     bx,2
        mov     fs,ax
        mov     ax,[fs:N_PARENT]
        test    ax,ax
        jz      .down
        cmp     bx,2*64
        jb      .up
        mov     [np_cut],1              ; deeper than any walk goes
.down:  sub     bx,2
        mov     fs,[chain+bx]
        cmp     di,[np_start]
        je      .name
        cmp     byte [di-1],'\'
        je      .name
        mov     ax,di
        sub     ax,[np_start]
        cmp     ax,NP_MAX
        jae     .cut
        mov     al,'\'
        stosb
.name:  movzx   cx,byte [fs:N_NLEN]
        mov     si,N_NAME
        cmp     [np_disk],0
        je      .fit
        test    byte [fs:N_FLAGS],F_SHORT
        jz      .fit
        add     si,cx                   ; the short name, after the long one's NUL
        inc     si
        push    si
        xor     cx,cx
@@:     cmp     byte [fs:si],0
        je      @f
        inc     cx
        inc     si
        jmp     @b
@@:     pop     si
.fit:   mov     ax,di
        sub     ax,[np_start]
        add     ax,cx
        cmp     ax,NP_MAX
        jbe     .copy
        mov     [np_cut],1
        mov     cx,[np_start]
        add     cx,NP_MAX
        sub     cx,di
.copy:  jcxz    .nx
@@:     mov     al,[fs:si]
        stosb
        inc     si
        loop    @b
.nx:    cmp     [np_cut],0
        jne     .end
        test    bx,bx
        jnz     .down
        jmp     .end
.cut:   mov     [np_cut],1
.end:   mov     byte [di],0
        mov     [np_disk],0
        mov     al,[np_cut]
        neg     al                      ; CF=1 if it was cut
        pop     fs
        pop     si
        pop     cx
        pop     bx
        pop     ax
        ret

; ---------------------------------------------------------------------------------------------
; input

; al = character, ah = scan code. CF=1 to quit.
handle_key:
        cmp     al,'q'
        je      .quit
        cmp     al,'Q'
        je      .quit
        cmp     al,0Dh
        je      .enter
        cmp     al,1Bh
        je      .up
        cmp     al,08h
        je      .up
        cmp     al,'+'
        je      .zin
        cmp     al,'='
        je      .zin
        cmp     al,'-'
        je      .zout
        cmp     al,'0'
        je      .zreset
        or      al,al
        jz      .ext
        cmp     al,0E0h
        je      .ext
        cmp     al,'R'
        je      .rescan
        or      al,20h
        cmp     al,'r'
        je      .rescan
        cmp     al,'a'
        je      .size
        cmp     al,'d'
        je      .del
        cmp     al,'h'
        je      .left
        cmp     al,'j'
        je      .down
        cmp     al,'k'
        je      .upk
        cmp     al,'l'
        je      .right
        clc
        ret
.ext:   cmp     ah,48h
        je      .upk
        cmp     ah,50h
        je      .down
        cmp     ah,4Bh
        je      .left
        cmp     ah,4Dh
        je      .right
        cmp     ah,53h
        je      .del
        clc
        ret
.quit:  stc
        ret
.enter: call    enter_sel
        clc
        ret
.up:    call    go_up
        clc
        ret
.zin:   mov     ax,[zoom]
        cmp     ax,[nfiles]
        jae     .r
        inc     [zoom]
        call    fill
        clc
        ret
.zout:  cmp     [zoom],0
        je      .r
        dec     [zoom]
        call    fill
        clc
        ret
.zreset:
        mov     [zoom],0
        call    fill
        clc
        ret
.size:  xor     [apparent],1
        mov     [size_off],N_DISK
        cmp     [apparent],0
        je      @f
        mov     [size_off],N_SIZE
@@:     call    change_files
        clc
        ret
.del:   call    delete_sel
        clc
        ret
.rescan:
        call    rescan_key
        clc
        ret
.right: mov     al,0
        jmp     .move
.left:  mov     al,1
        jmp     .move
.down:  mov     al,2
        jmp     .move
.upk:   mov     al,3
.move:  call    move_sel
.r:     clc
        ret

; al = 0 right, 1 left, 2 down, 3 up (Board::move_selected_*): the adjacent tile that overlaps
; the selected one most; off the edge, nothing
move_sel:
        mov     [cursor_chosen],1
        cmp     [ntiles],0
        je      .r
        cmp     [sel],-1
        jne     .have
        mov     [sel],0
.r:     ret
.have:
        mov     [mv_fwd],1
        test    al,1
        jz      @f
        mov     [mv_fwd],0
@@:     cmp     al,2
        jae     .vert
        mov     [mv_a],T_X
        mov     [mv_la],T_W
        mov     [mv_o],T_Y
        mov     [mv_lo],T_H
        jmp     .go
.vert:  mov     [mv_a],T_Y
        mov     [mv_la],T_H
        mov     [mv_o],T_X
        mov     [mv_lo],T_W
.go:    mov     si,[sel]
        shl     si,4
        add     si,tiles
        mov     bx,[mv_a]
        mov     ax,[si+bx]
        mov     [s_a],ax
        mov     bx,[mv_la]
        add     ax,[si+bx]
        mov     [s_ae],ax
        mov     bx,[mv_o]
        mov     ax,[si+bx]
        mov     [s_o],ax
        mov     bx,[mv_lo]
        add     ax,[si+bx]
        mov     [s_oe],ax
        mov     [best],-1
        xor     cx,cx
.c:     cmp     cx,[ntiles]
        jae     .chosen
        mov     di,cx
        shl     di,4
        add     di,tiles
        mov     bx,[mv_a]
        mov     ax,[di+bx]              ; c.a
        mov     bx,[mv_la]
        mov     dx,ax
        add     dx,[di+bx]              ; c.a + c.la
        cmp     [mv_fwd],0
        je      .back
        cmp     ax,[s_ae]
        jne     .nx
        jmp     .adj
.back:  cmp     dx,[s_a]
        jne     .nx
.adj:   mov     bx,[mv_o]
        mov     ax,[di+bx]              ; c.o
        mov     bx,[mv_lo]
        mov     dx,ax
        add     dx,[di+bx]              ; c.oe
        ; overlaps (horizontally_overlaps_with / vertically_overlaps_with)
        cmp     ax,[s_o]
        jb      @f
        cmp     ax,[s_oe]
        jbe     .ov
@@:     cmp     dx,[s_oe]
        ja      @f
        cmp     dx,[s_o]
        ja      .ov
@@:     cmp     ax,[s_o]
        ja      @f
        cmp     dx,[s_oe]
        jae     .ov
@@:     mov     bx,[s_o]
        cmp     bx,ax
        ja      .nx
        cmp     [s_oe],dx
        jb      .nx
.ov:    ; min(ends) - max(starts)
        cmp     dx,[s_oe]
        jbe     @f
        mov     dx,[s_oe]
@@:     cmp     ax,[s_o]
        jae     @f
        mov     ax,[s_o]
@@:     sub     dx,ax
        cmp     [best],-1
        je      .take
        cmp     dx,[best_ov]
        jb      .nx
.take:  mov     [best],cx
        mov     [best_ov],dx
.nx:    inc     cx
        jmp     .c
.chosen:
        mov     ax,[best]
        mov     [sel],ax
        ret

enter_sel:
        mov     bx,[sel]
        cmp     bx,-1
        je      .r
        shl     bx,4
        add     bx,tiles
        mov     ax,[bx+T_NODE]
        mov     fs,ax
        test    byte [fs:N_FLAGS],F_DIR
        jz      .r
        mov     bx,[navsp]
        cmp     bx,64*4
        jae     .r
        mov     dx,[sel]
        mov     [navstack+bx],dx
        mov     dx,[zoom]
        mov     [navstack+bx+2],dx
        add     [navsp],4
        mov     [cur],ax
        mov     [zoom],0
        mov     [sel],-1
        mov     [cursor_chosen],0
        call    change_files
.r:     ret

go_up:
        mov     ax,[cur]
        cmp     ax,[root]
        je      .r
        mov     fs,ax
        mov     ax,[fs:N_PARENT]
        mov     [cur],ax
        mov     dx,-1
        xor     cx,cx
        mov     bx,[navsp]
        test    bx,bx
        jz      @f
        sub     bx,4
        mov     [navsp],bx
        mov     dx,[navstack+bx]
        mov     cx,[navstack+bx+2]
@@:     mov     [zoom],cx
        mov     [sel],dx
        mov     [cursor_chosen],0
        call    change_files
.r:     ret

; a press of the left button selects the tile under it; a second on the same tile within
; half a second enters it
mouse_poll:
        cmp     [mouse],0
        je      .no
        mov     ax,5
        xor     bx,bx
        int     33h
        test    bx,bx
        jz      .no
        mov     [presses],bx
        shr     cx,3
        shr     dx,3
        call    tile_at
        cmp     ax,-1
        je      .no
        mov     [cursor_chosen],1
        cmp     [presses],2             ; both clicks of a double click since the last look
        jb      @f
        mov     [sel],ax
        jmp     .enter
@@:     push    ax
        call    ticks
        mov     edx,eax
        pop     ax
        cmp     ax,[sel]
        jne     .select
        cmp     ax,[click_tile]
        jne     .select
        mov     ecx,edx
        sub     ecx,[click_time]
        cmp     ecx,9
        ja      .select
.enter: mov     [click_tile],-1
        call    enter_sel
        stc
        ret
.select:
        mov     [sel],ax
        mov     [click_tile],ax
        mov     [click_time],edx
        stc
        ret
.no:    clc
        ret

; cx = column, dx = row → ax = tile, or -1 (Board::tile_at)
tile_at:
        xor     ax,ax
        mov     si,tiles
.l:     cmp     ax,[ntiles]
        jae     .none
        mov     bx,[si+T_X]
        cmp     cx,bx
        jb      .n
        add     bx,[si+T_W]
        cmp     cx,bx
        jae     .n
        mov     bx,[si+T_Y]
        cmp     dx,bx
        jb      .n
        add     bx,[si+T_H]
        cmp     dx,bx
        jb      .r
.n:     inc     ax
        add     si,T_SIZE
        jmp     .l
.none:  mov     ax,-1
.r:     ret

; ---------------------------------------------------------------------------------------------
; deleting

delete_sel:
        cmp     [cursor_chosen],0       ; never an entry nobody picked
        je      .r
        mov     bx,[sel]
        cmp     bx,-1
        je      .r
        shl     bx,4
        add     bx,tiles
        mov     ax,[bx+T_NODE]
        mov     [del_node],ax
        mov     bx,[bx+T_IDX]
        mov     [del_idx],bx
        mov     fs,ax
        test    byte [fs:N_FLAGS],F_PSEUDO
        jz      .ask
        call    render
        call    dialog_box
        mov     si,s_pseudo1
        mov     dh,10
        call    dialog_line_z
        mov     si,s_pseudo2
        mov     dh,11
        call    dialog_line_z
        mov     si,s_anykey
        mov     dh,13
        call    dialog_line_z
        call    blit
        call    read_key
.r:     ret
.ask:
        call    render
        call    dialog_box
        mov     si,s_del_file
        mov     fs,[del_node]
        test    byte [fs:N_FLAGS],F_DIR
        jz      @f
        mov     si,s_del_dir
@@:     mov     dh,9
        call    dialog_line_z
        ; the name
        movzx   cx,byte [fs:N_NLEN]
        mov     di,l1buf
        xor     si,si
        push    cx
@@:     jcxz    @f
        mov     al,[fs:N_NAME+si]
        stosb
        inc     si
        dec     cx
        jmp     @b
@@:     pop     cx
        mov     si,l1buf
        mov     dx,54
        call    truncate_middle
        mov     si,l1buf
        mov     dh,11
        call    dialog_line
        ; its size
        mov     di,tb
        mov     bx,[del_idx]
        call    load_size
        call    fmt_size
        mov     fs,[del_node]
        test    byte [fs:N_FLAGS],F_DIR
        jz      @f
        mov     si,s_lparen
        call    append_z
        mov     eax,[fs:N_DESC]
        call    u32dec_c
        mov     si,s_files_close
        call    append_z
@@:     mov     cx,di
        sub     cx,tb
        mov     si,tb
        mov     dh,12
        call    dialog_line
        mov     si,s_yes_no
        mov     dh,14
        call    dialog_line_z
        call    blit
        call    read_key
        or      al,20h
        cmp     al,'y'
        jne     .r2
        call    dialog_box
        mov     si,s_deleting
        mov     dh,11
        call    dialog_line_z
        call    blit
        mov     di,pathbuf
        mov     ax,[del_node]
        mov     [np_disk],1
        call    node_path
        mov     cx,di
        sub     cx,pathbuf
        mov     [pathlen],cx
        mov     ax,3                    ; path not found
        jc      .failed
        mov     [rm_depth],0
        mov     fs,[del_node]
        test    byte [fs:N_FLAGS],F_DIR
        jz      .file
        call    rm_tree
        jmp     .did
.file:  call    rm_file
.did:   jc      .failed
        call    tree_remove
        mov     [cursor_chosen],0       ; its neighbour is in hand now, not picked
        call    change_files
.r2:    ret
.failed:
        mov     [del_err],ax
        mov     fs,[del_node]           ; some of a folder may have gone: walk it again
        test    byte [fs:N_FLAGS],F_DIR
        jz      @f
        mov     [rs_freed],1
        mov     ax,[del_node]
        call    rescan_node
        call    change_files
@@:     call    render
        call    dialog_box
        mov     si,s_failed
        mov     dh,10
        call    dialog_line_z
        mov     di,tb
        mov     si,s_doserr
        call    append_z
        movzx   eax,[del_err]
        call    u32dec
        mov     cx,di
        sub     cx,tb
        mov     si,tb
        mov     dh,11
        call    dialog_line
        mov     si,s_anykey
        mov     dh,13
        call    dialog_line_z
        call    blit
        call    read_key
        ret

dialog_box:
        mov     dl,10
        mov     dh,8
        mov     cl,60
        mov     ch,8
        mov     ax,A_DIALOG*256+' '
        call    fill_cells
        mov     ah,A_DIALOG
        mov     al,0C9h
        mov     dl,10
        mov     dh,8
        call    put_ch
        mov     al,0BBh
        mov     dl,69
        call    put_ch
        mov     al,0C8h
        mov     dl,10
        mov     dh,15
        call    put_ch
        mov     al,0BCh
        mov     dl,69
        call    put_ch
        mov     dl,11
.h:     mov     al,0CDh
        mov     dh,8
        call    put_ch
        mov     dh,15
        call    put_ch
        inc     dl
        cmp     dl,69
        jb      .h
        mov     dh,9
.v:     mov     al,0BAh
        mov     dl,10
        call    put_ch
        mov     dl,69
        call    put_ch
        inc     dh
        cmp     dh,15
        jb      .v
        ret

; ds:si zero-terminated, centred on row dh of the dialog
dialog_line_z:
        push    si
        xor     cx,cx
@@:     lodsb
        test    al,al
        jz      @f
        inc     cx
        jmp     @b
@@:     pop     si
; ds:si, cx = length
dialog_line:
        mov     ax,80
        sub     ax,cx
        shr     ax,1
        mov     dl,al
        mov     ah,A_DIALOG
        call    put_str
        ret

; pathbuf: the file. If DOS refuses, its read-only, hidden and system bits are cleared and it is
; tried again (only then: DOSBox-X keeps a host file's attributes in a file of their own).
; CF=1 and ax = error on failure.
rm_file:
        call    .del
        jnc     .r
        push    ax
        call    rm_attrs
        pop     ax
        jc      .fail                   ; nothing to clear: the first error stands
        call    .del
.r:     ret
.fail:  stc
        ret
.del:   mov     dx,pathbuf
        cmp     [lfn],0
        je      .s
        mov     ax,7141h
        xor     si,si
        stc
        int     21h
        ret
.s:     mov     ah,41h
        int     21h
        ret

rm_dir:
        mov     dx,pathbuf
        cmp     [lfn],0
        je      .s
        mov     ax,713Ah
        stc
        int     21h
        ret
.s:     mov     ah,3Ah
        int     21h
        ret

; pathbuf: the folder and everything in it, carrying on past failures. CF=1, ax = the first
; error. Its entries are queued and removed after the search is closed, like the scan's; a
; folder deeper than RM_MAXDEPTH is left, with an error, rather than run out of stack.
rm_tree:
        push    bp
        mov     bp,sp
        sub     sp,FRAME
        mov     word [bp-S_PSEUDO],0
        mov     ax,[pend_top]
        mov     [bp-S_PEND],ax
        mov     bx,[pathlen]
        mov     [bp-S_SAVED],bx
        inc     [rm_depth]
        cmp     [rm_depth],RM_MAXDEPTH
        jbe     @f
        mov     word [bp-S_PSEUDO],3
        jmp     .done
@@:     cmp     byte [pathbuf+bx-1],'\'
        je      @f
        mov     byte [pathbuf+bx],'\'
        inc     bx
@@:     mov     [bp-S_BASE],bx
        mov     dword [pathbuf+bx],'*.*'
        mov     dx,pathbuf
        lea     di,[bp-FD]
        call    ff_first
        jc      .rmdir
        mov     [bp-S_HANDLE],ax
.e:     lea     di,[bp-FD]
        call    extract
        call    is_dots
        je      .n
        xor     ax,ax                   ; 0: a folder, 1: a file
        test    [ent_attr],10h
        jnz     @f
        inc     ax
@@:     call    push_pending
        jnc     .n
        mov     ax,8                    ; no room to queue it: not enough memory
        cmp     word [bp-S_PSEUDO],0
        jne     .n
        mov     [bp-S_PSEUDO],ax
.n:     mov     bx,[bp-S_HANDLE]
        lea     di,[bp-FD]
        call    ff_next
        jnc     .e
        mov     bx,[bp-S_HANDLE]
        call    ff_close
        mov     ax,[pend_top]
        mov     [bp-S_END],ax
        mov     si,[bp-S_PEND]
.sub:   cmp     si,[bp-S_END]
        jae     .subs_done
        mov     bx,[bp-S_BASE]          ; the queued name must fit in the path
        push    fs
        mov     fs,[pendseg]
        movzx   ax,byte [fs:si+2]
        pop     fs
        add     ax,bx
        cmp     ax,PATHMAX
        jb      @f
        push    fs                      ; skip it
        mov     fs,[pendseg]
        movzx   ax,byte [fs:si+2]
        pop     fs
        add     si,ax
        add     si,3
        mov     ax,3
        jmp     .err
@@:     call    pop_pending
        mov     [bp-S_CUR],si
        test    ax,ax
        jnz     .file
        call    rm_tree
        jmp     .chk
.file:  call    rm_file
.chk:   mov     si,[bp-S_CUR]
        jnc     .sub
.err:   cmp     word [bp-S_PSEUDO],0
        jne     .sub
        mov     [bp-S_PSEUDO],ax
        jmp     .sub
.subs_done:
        mov     ax,[bp-S_PEND]
        mov     [pend_top],ax
.rmdir:
        mov     bx,[bp-S_SAVED]
        mov     [pathlen],bx
        mov     byte [pathbuf+bx],0
        call    rm_dir
        jnc     .done
        push    ax                      ; a read-only or hidden folder: cleared, and again
        call    rm_attrs
        pop     ax
        jc      .err2
        call    rm_dir
        jnc     .done
.err2:
        cmp     word [bp-S_PSEUDO],0
        jne     .done
        mov     [bp-S_PSEUDO],ax
.done:  mov     bx,[bp-S_SAVED]
        mov     [pathlen],bx
        mov     byte [pathbuf+bx],0
        dec     [rm_depth]
        mov     ax,[bp-S_PSEUDO]
        cmp     ax,1                    ; CF=1 if an error was recorded
        cmc
        mov     sp,bp
        pop     bp
        ret

; pathbuf's read-only, hidden and system bits cleared, the others kept. CF=1 if none was set
; (or they could not be read or written).
rm_attrs:
        mov     dx,pathbuf
        cmp     [lfn],0
        je      .s
        mov     ax,7143h
        xor     bl,bl                   ; get
        stc
        int     21h
        jc      .s
        test    cl,7
        jz      .none
        and     cx,0FFF8h
        mov     ax,7143h
        mov     bl,1                    ; set
        stc
        int     21h
        ret
.s:     mov     ax,4300h
        int     21h
        jc      .r
        test    cl,7
        jz      .none
        and     cx,0FFE8h               ; and not the folder bit, which 4301h refuses
        mov     ax,4301h
        int     21h
.r:     ret
.none:  stc
        ret

; ax = a folder: walked again from disk into a new node in its place (the Rescanner), which is
; returned in ax (0 if the folder has gone). pathbuf = its path, as DOS needs it. With rs_freed
; set, what it lost since counts as freed. The old node is leaked, like everything taken out.
rescan_node:
        mov     [rs_old],ax
        mov     fs,ax
        mov     bx,[fs:N_PARENT]
        mov     [rs_parent],bx
        test    bx,bx
        jz      .root
        mov     [del_node],ax
        call    tree_remove             ; freed += all of it, for now
        call    probe_dir
        jc      .gone
        mov     fs,[rs_old]             ; a node like it
        movzx   cx,byte [fs:N_NLEN]
        test    byte [fs:N_FLAGS],F_SHORT
        jz      @f
        add     cx,14
@@:     mov     al,1
        call    alloc_node
        jc      .gone
        mov     [rs_new],ax
        mov     fs,[rs_old]
        movzx   cx,byte [fs:N_NLEN]     ; the name, its NUL, and a short name after it
        inc     cx
        test    byte [fs:N_FLAGS],F_SHORT
        jz      @f
        add     cx,14
@@:     push    ds
        push    es
        mov     es,ax
        mov     ds,[rs_old]
        mov     si,N_FLAGS
        mov     di,N_FLAGS
        movsw                           ; the flags and the name's length
        mov     si,N_NAME
        mov     di,N_NAME
        rep     movsb
        pop     es
        pop     ds
        mov     es,ax
        and     byte [es:N_FLAGS],F_DIR+F_SHORT
        mov     bx,[rs_parent]
        mov     [es:N_PARENT],bx
        mov     fs,bx
        mov     dx,[fs:N_CHILD]
        mov     [es:N_NEXT],dx
        mov     [fs:N_CHILD],ax
        inc     word [fs:N_COUNT]
        push    ds
        pop     es
        xor     eax,eax
        mov     dword [add_app],eax
        mov     dword [add_app+4],eax
        mov     dword [add_disk],eax
        mov     dword [add_disk+4],eax
        mov     [add_desc],1
        mov     ax,bx
        call    add_up
        jmp     .walk
.root:  mov     cx,[pathlen]            ; everything: a new root
        mov     al,1
        call    alloc_node
        jc      .fail
        mov     [rs_new],ax
        mov     es,ax
        mov     byte [es:N_FLAGS],F_DIR
        mov     si,pathbuf
        mov     cx,[pathlen]
        call    set_name
        push    ds
        pop     es
.walk:  mov     ax,[rs_new]             ; its depth, so the walk stops where the first one did
        xor     cx,cx
@@:     mov     fs,ax
        mov     ax,[fs:N_PARENT]
        inc     cx
        test    ax,ax
        jnz     @b
        dec     cx
        mov     [depth],cx
        mov     ax,[rs_new]
        call    scan_dir
        mov     [depth],0
        cmp     [rs_parent],0
        jne     @f
        mov     ax,[rs_new]
        mov     [root],ax
        jmp     .done
@@:     mov     fs,[rs_new]             ; freed: what it lost, or nothing
        cmp     [rs_freed],0
        jne     .sub
        mov     fs,[rs_old]
.sub:   mov     eax,[fs:N_SIZE]
        sub     dword [freed+N_SIZE],eax
        mov     eax,[fs:N_SIZE+4]
        sbb     dword [freed+N_SIZE+4],eax
        mov     eax,[fs:N_DISK]
        sub     dword [freed+N_DISK],eax
        mov     eax,[fs:N_DISK+4]
        sbb     dword [freed+N_DISK+4],eax
.done:  mov     [rs_freed],0
        mov     ax,[rs_new]
        ret
.gone:  cmp     [rs_freed],0            ; all of it gone: freed if we deleted it
        jne     .fail
        mov     fs,[rs_old]
        mov     eax,[fs:N_SIZE]
        sub     dword [freed+N_SIZE],eax
        mov     eax,[fs:N_SIZE+4]
        sbb     dword [freed+N_SIZE+4],eax
        mov     eax,[fs:N_DISK]
        sub     dword [freed+N_DISK],eax
        mov     eax,[fs:N_DISK+4]
        sbb     dword [freed+N_DISK+4],eax
.fail:  mov     [rs_freed],0
        xor     ax,ax
        ret

; al = 'r': the selected folder, else the one shown, scanned again; 'R': everything
rescan_key:
        mov     bx,[root]
        cmp     al,'R'
        je      .go
        mov     bx,[cur]
        mov     si,[sel]
        cmp     si,-1
        je      .go
        shl     si,4
        mov     ax,word [tiles+si+T_NODE]
        mov     fs,ax
        test    byte [fs:N_FLAGS],F_DIR
        jz      .go
        mov     bx,ax
.go:    mov     [rs_target],bx
        call    render
        call    dialog_box
        mov     si,s_rescanning
        mov     dh,11
        call    dialog_line_z
        call    blit
        mov     di,pathbuf
        mov     ax,[rs_target]
        mov     [np_disk],1
        call    node_path
        mov     cx,di
        sub     cx,pathbuf
        mov     [pathlen],cx
        jc      .r
        mov     ax,[rs_target]
        call    rescan_node
        mov     bx,[rs_target]
        cmp     bx,[root]
        je      .all
        cmp     bx,[cur]
        jne     .shown
        test    ax,ax                   ; the folder shown: the new one, or its parent if gone
        jnz     @f
        mov     ax,[rs_parent]
@@:     mov     [cur],ax
        mov     [list_cur],0
        jmp     .shown
.all:   test    ax,ax
        jz      .shown
        mov     [root],ax               ; everything: back to the top
        mov     [cur],ax
        mov     [navsp],0
        mov     [zoom],0
        mov     [list_cur],0
.shown: mov     [sel],-1
        mov     [cursor_chosen],0
        call    change_files
.r:     ret

; take del_node out of the tree and its size off every folder above it
tree_remove:
        mov     fs,[del_node]
        mov     eax,[fs:N_SIZE]
        mov     edx,[fs:N_SIZE+4]
        add     dword [freed+N_SIZE],eax
        adc     dword [freed+N_SIZE+4],edx
        not     eax
        not     edx
        add     eax,1
        adc     edx,0
        mov     dword [add_app],eax
        mov     dword [add_app+4],edx
        mov     eax,[fs:N_DISK]
        mov     edx,[fs:N_DISK+4]
        add     dword [freed+N_DISK],eax
        adc     dword [freed+N_DISK+4],edx
        not     eax
        not     edx
        add     eax,1
        adc     edx,0
        mov     dword [add_disk],eax
        mov     dword [add_disk+4],edx
        mov     eax,[fs:N_DESC]
        inc     eax
        neg     eax
        mov     [add_desc],eax
        mov     ax,[fs:N_PARENT]
        call    add_up
        mov     dx,[fs:N_NEXT]
        mov     bx,[del_node]
        mov     fs,ax
        dec     word [fs:N_COUNT]
        mov     ax,[fs:N_CHILD]
        cmp     ax,bx
        jne     .walk
        mov     [fs:N_CHILD],dx
        ret
.walk:  test    ax,ax
        jz      .r
        mov     fs,ax
        mov     ax,[fs:N_NEXT]
        cmp     ax,bx
        jne     .walk
        mov     [fs:N_NEXT],dx
.r:     ret

; =============================================================================================

segment dseg use16

psp             dw 0
lfn             db 0
video           db 0
mouse           db 0
abort           db 0
scanning        db 0
apparent        db 0
spin            db 0
recurse_ok      db 0
size_off        dw N_DISK
clus            dd 512
entseg          dw 0
pendseg         dw 0
pend_top        dw 0
arena_next      dw 0
arena_end       dw 0
arena_limit     dw 0
root            dw 0
cur             dw 0
nent            dw 0
nfiles          dw 0
zoom            dw 0
sel             dw -1
depth           dw 0
seen            dd 0
last_tick       dd 0
freed           dq 0,0                  ; apparent, on disk: as a node's sizes
navsp           dw 0
click_tile      dw -1
presses         dw 0
click_time      dd 0
del_node        dw 0
del_idx         dw 0
del_err         dw 0
rm_depth        dw 0
rs_old          dw 0
rs_new          dw 0
rs_parent       dw 0
rs_freed        db 0
rs_target       dw 0
list_cur        dw 0            ; the list's cursor, by node (PANEL.ASM)
cursor_chosen   db 0            ; the entry in hand was picked, not placed by the program

; the board: the treemap's bounds; its borders reach column 79 and row 22
bnd_x           dw 0
bnd_y           dw 1
bnd_w           dw 79
bnd_h           dw 21

cw_near         dw 027Fh                ; double precision, round to nearest, all masked
cw_chop         dw 0E7Fh                ; the same, truncating
c_ratio         dq 2.5
c_half          dq 0.5
c_minh          dq 3.0
c_minw          dq 8.0
c_65535         dq 65535.0
c_100           dq 100.0
c_ten           dq 10.0
c_g             dq 999999999.0
c_m             dq 999999.0
c_k             dq 999.0
c_gib           dq 1073741824.0
c_mib           dq 1048576.0
c_kib           dq 1024.0

gaps            dw 10941,4376,1750,701,301,132,57,23,10,4,1,0

; CP437 box drawing by direction bits: up 1, down 2, left 4, right 8
boxch           db ' ',0B3h,0B3h,0B3h,0C4h,0D9h,0BFh,0B4h
                db 0C4h,0C0h,0DAh,0C3h,0C4h,0C1h,0C2h,0C5h
spinner         db '|/-\'

pseudo_name     db '(not in memory)'
pseudo_len      = $ - pseudo_name

s_total         db ' Total',0
s_scanning      db ' Scanning',0
s_apparent      db ' (apparent)',0
s_colon         db ': ',0
s_lparen        db ' (',0
s_comma         db ', ',0
s_files_close   db ' files)',0
s_files         db ' files',0
s_freed         db ', freed: ',0
s_dots          db '...'
s_selected      db ' SELECTED: ',0
s_sep           db '  ',0B3h,'  ',0
s_skipped       db '  ',0B3h,'  not scanned: too deep',0
s_pct_close     db '%)',0
s_scanning_path db ' ',0
s_hint          db ' Select a tile with the arrow keys, hjkl or the mouse',0
s_zoom          db ' zoom ',0
s_empty         db 'Folder is empty'
s_descendants   db ' descendants)',0
s_trunc_even    db '[...]',0
s_trunc_odd     db '[..]',0
s_legend        db ' ~',1Bh,19h,18h,1Ah,'/hjkl~ move  ~Enter~ open  ~Esc~ up  ~+ - 0~ zoom'
                db '  ~a~ apparent  ~d~ delete  ~q~ quit',0
s_legend_a      db ' ~',1Bh,19h,18h,1Ah,'/hjkl~ move  ~Enter~ open  ~Esc~ up  ~+ - 0~ zoom'
                db '  ~a~ on disk  ~d~ delete  ~q~ quit',0
s_del_file      db 'Delete this file?',0
s_del_dir       db 'Delete this folder and everything in it?',0
s_yes_no        db '(y) Yes          (n) No',0
s_deleting      db 'Deleting...',0
s_rescanning    db 'Scanning again...',0
s_failed        db 'Could not delete all of it.',0
s_doserr        db 'DOS error ',0
s_anykey        db 'Press any key',0
s_pseudo1       db 'This stands for files there was no memory to list.',0
s_pseudo2       db 'Scan a folder further down to see them.',0

msg_usage       db 'diskonaut for DOS: a treemap of what fills your disk.',13,10,13,10
                db 'usage: DISKONAU [-a] [-k KEYS] [-s FILE] [FOLDER]',13,10
                db '  -a       show apparent sizes instead of space on disk',13,10
                db '  -k KEYS  type these first (E is Enter, X is Esc)',13,10
                db '  -s FILE  then write the screen to FILE and quit',13,10,'$'
msg_notdir      db 'diskonaut: not a folder',13,10,'$'
msg_nomem       db 'diskonaut: not enough memory',13,10,'$'
msg_cpu         db 'diskonaut: needs a 386 or later',13,10,'$'
msg_fpu         db 'diskonaut: needs a math coprocessor (387)',13,10,'$'

; the treemap's working values
em_x            dq 0.0
em_y            dq 0.0
em_w            dq 0.0
em_h            dq 0.0
tot_area        dq 0.0
ftotal          dq 0.0
eq_pct          dq 0.0
len             dq 0.0
min1            dq 0.0
min2            dq 0.0
min_prod        dq 0.0
w_sum           dq 0.0
w_size          dq 0.0
w_first         dq 0.0
w_second        dq 0.0
w_worst         dq 0.0
w_cur           dq 0.0
w_nxt           dq 0.0
l_total         dq 0.0
l_prog          dq 0.0
l_second        dq 0.0
l_tsecond       dq 0.0
l_size          dq 0.0
l_first         dq 0.0
r_x             dq 0.0
r_y             dq 0.0
r_w             dq 0.0
r_h             dq 0.0
tmp64           dq 0
tmp32           dd 0
add_app         dq 0
add_disk        dq 0
add_desc        dd 0
ent_size        dq 0
pctv            dd 0
pctv_even       dd 0
equal           db 0
w_have          db 0
l_horiz         db 0
ent_attr        db 0
mv_fwd          db 0
a1              db 0
a2              db 0
r1              db 0
r2              db 0
rs              dw 0
ri              dw 0
w_a             dw 0
w_b             dw 0
l_a             dw 0
l_b             dw 0
t_x             dw 0
t_y             dw 0
t_w             dw 0
t_h             dw 0
gap             dw 0
tmp_e           dw 0
mv_a            dw 0
mv_la           dw 0
mv_o            dw 0
mv_lo           dw 0
s_a             dw 0
s_ae            dw 0
s_o             dw 0
s_oe            dw 0
best            dw 0
best_ov         dw 0
dt_tile         dw 0
maxlen          dw 0
fnlen           dw 0
ind_short       dw 0
l1len           dw 0
l2len           dw 0
s1len           dw 0
s2len           dw 0
title_len       dw 0
np_start        dw 0
np_disk         db 0
np_cut          db 0
ent_lossy       db 0
ent_cx          dw 0
shortlen        dw 0
pathlen         dw 0
namelen         dw 0
keyp            dw 0
snap            db 0

args            rb 130
snapbuf         rb 130
keysbuf         rb 130
navstack        rw 2*64
chain           rw 64
pathbuf         rb 300
namebuf         rb 300
shortbuf        rb 16
tb              rb 400
l1buf           rb 300
l2buf           rb 64
indbuf          rb 64
trbuf           rb 300
s1buf           rb 32
s2buf           rb 32
ntiles          dw 0
small_x         dw 0
small_y         dw 0
has_small       db 0,0
tiles           rb MAXTILES*T_SIZE
tiles_end:
boxbits         rb 256
scr             rw 80*25
