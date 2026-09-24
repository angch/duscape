; diskonaut for MS-DOS: the treemap viewer as a 16-bit real-mode program.
;
; Assembled by FASM (the DOS build of flat assembler 1.73) into an MZ executable, see BUILD.BAT
; and `make dos`. It runs on a 286 with no coprocessor: 16-bit registers only, no FS or GS, no
; near conditional jumps (J286.INC makes them short ones over a jmp), and the treemap's doubles in
; software (SOFTFP.ASM), so it lays out exactly as the Rust f64 code does.
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
include 'J286.INC'
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
        call    f_constants
        mov     di,boxbits              ; box character → the directions it joins
        xor     ax,ax
        mov     cx,128
        rep     stosw
        mov     bx,1
@@:     mov     al,[boxch+bx]
        xor     ah,ah
        mov     si,ax
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
        mov     [last_tick],ax
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
        cmp     [pv_state],PV_PENDING   ; a picture, once the keyboard has rested 100 ms
        jne     .snap
        cmp     [panel_on],0
        je      .snap
        call    ticks
        sub     ax,[last_input]
        cmp     ax,2
        jb      @f
        call    pv_decode
        jmp     main_loop
.snap:  cmp     [snap],0
        je      @f
        call    write_snap
        jmp     quit
@@:     call    mouse_poll
        jc      main_loop
        hlt
        jmp     .wait
.key:
        call    read_key
        push    ax
        call    ticks
        mov     [last_input],ax
        pop     ax
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

; an 80186 or later (pusha, shifts by a count, immediates pushed), else a message and out. An
; 8086 or 8088 shifts by the whole of CL; the 186 and after by its low five bits.
check_cpu:
        mov     cl,32
        mov     ax,1
        shl     ax,cl
        test    ax,ax
        jz      .old
        ret
.old:   mov     dx,msg_cpu
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
        mov     cl,[es:80h]
        xor     ch,ch
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

; the next key: from -k first, else the keyboard; ax as INT 16h gives it. In -k, E is Enter and
; X Esc (DOS takes < and > for itself), T Tab, P and N Page Up and Down, H Home, Z End.
script_keys     db 'E',0Dh,1Ch, 'X',1Bh,01h, 'T',09h,0Fh, 'P',0,49h, 'N',0,51h, 'H',0,47h
                db 'Z',0,4Fh, 0
read_key:
        mov     si,[keyp]
        test    si,si
        jz      .bios
        lodsb
        test    al,al
        jz      .end
        mov     [keyp],si
        xor     ah,ah
        mov     si,script_keys
@@:     cmp     byte [cs:si],0
        je      .r
        cmp     al,[cs:si]
        je      @f
        add     si,3
        jmp     @b
@@:     mov     ax,[cs:si+1]
.r:     ret
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
        mov     ah,40h                  ; and the 16 colours as they are now, eight-bit RGB
        mov     cx,48
        mov     dx,pal_orig
        cmp     [pal_applied],0
        je      @f
        mov     dx,pal_pic
@@:     int     21h
        mov     ah,40h                  ; and the picture: its state, size, and each block
        mov     cx,1                    ; pixel's colour before it is matched to those 16
        mov     dx,pv_state
        int     21h
        mov     ah,40h
        mov     cx,4
        mov     dx,acc_cols
        int     21h
        mov     ah,40h
        mov     cx,PV_COLS*PV_HROWS*4
        mov     dx,tcol
        int     21h
        mov     ah,40h
        mov     cx,PV_COLS*PV_HROWS
        mov     dx,blk
        int     21h
        mov     ah,40h                  ; and how big the arena is, in paragraphs
        mov     cx,2
        mov     dx,arena_paras
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
@@:     mov     word [pathbuf+bx],'*.'
        mov     word [pathbuf+bx+2],'*'
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
        push    ax                      ; the space used on the drive, for the side panel:
        push    cx                      ; used clusters × sectors a cluster × bytes a sector
        push    dx
        sub     dx,bx
        mov     bx,dx
        mul     cx                      ; dx:ax, the bytes of a cluster
        mov     cx,dx
        mul     bx
        mov     word [disk_used],ax
        mov     word [disk_used+2],dx
        mov     ax,cx
        mul     bx
        add     word [disk_used+2],ax
        adc     dx,0
        mov     word [disk_used+4],dx
        mov     word [disk_used+6],0
        mov     [disk_known],1
        pop     dx
        pop     cx
        pop     ax
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
        mov     [winseg],ax             ; inflate's window: walks and pictures never overlap
        ; the preview's two picture lines, or a text file's head: in upper memory if DOS has it,
        ; to leave the more for the tree
        mov     ax,5800h
        int     21h
        mov     [old_strategy],ax
        mov     ax,5802h
        int     21h
        mov     [old_umb_link],al
        mov     ax,5803h                ; the upper memory blocks linked in
        mov     bx,1
        int     21h
        mov     ax,5801h                ; high first, then low
        mov     bx,0080h
        int     21h
        mov     ah,48h
        mov     bx,1000h
        int     21h
        pushf
        push    ax
        mov     ax,5801h
        mov     bx,[old_strategy]
        int     21h
        mov     ax,5803h
        mov     bl,[old_umb_link]
        xor     bh,bh
        int     21h
        pop     ax
        popf
        jc      nomem
        mov     [lineseg],ax
        mov     ah,48h
        mov     bx,0FFFFh
        int     21h
        cmp     bx,RESERVE+100h
        jb      nomem
        mov     ah,48h
        int     21h
        jc      nomem
        mov     [arena_next],ax
        mov     [arena_paras],bx
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
        call    pal_init
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
        push    bx
        push    es
.l:     test    ax,ax
        jz      .d
        mov     es,ax
        mov     bx,[add_app]
        add     [es:N_SIZE],bx
        mov     bx,[add_app+2]
        adc     [es:N_SIZE+2],bx
        mov     bx,[add_app+4]
        adc     [es:N_SIZE+4],bx
        mov     bx,[add_app+6]
        adc     [es:N_SIZE+6],bx
        mov     bx,[add_disk]
        add     [es:N_DISK],bx
        mov     bx,[add_disk+2]
        adc     [es:N_DISK+2],bx
        mov     bx,[add_disk+4]
        adc     [es:N_DISK+4],bx
        mov     bx,[add_disk+6]
        adc     [es:N_DISK+6],bx
        mov     bx,[add_desc]
        add     [es:N_DESC],bx
        mov     bx,[add_desc+2]
        adc     [es:N_DESC+2],bx
        mov     ax,[es:N_PARENT]
        jmp     .l
.d:     pop     es
        pop     bx
        pop     ax
        ret

; add_app rounded up to a whole number of clusters → add_disk. Clusters are a power of two in
; every DOS file system; were one not, the length is taken as it is.
round_to_cluster:
        mov     si,add_app
        mov     di,add_disk
        mov     cx,4
        rep     movsw
        mov     ax,[clus]
        mov     dx,[clus+2]
        sub     ax,1                    ; the mask: cluster - 1
        sbb     dx,0
        mov     bx,ax
        and     bx,[clus]
        mov     cx,dx
        and     cx,[clus+2]
        or      bx,cx
        jnz     .r                      ; not a power of two
        add     [add_disk],ax
        adc     [add_disk+2],dx
        adc     word [add_disk+4],0
        adc     word [add_disk+6],0
        not     ax
        not     dx
        and     [add_disk],ax
        and     [add_disk+2],dx
.r:     ret

; the child node ax linked in first among the children of bx; ES is kept
link_child:
        push    dx
        push    es
        mov     es,ax
        mov     [es:N_PARENT],bx
        mov     es,bx
        mov     dx,[es:N_CHILD]
        mov     [es:N_CHILD],ax
        inc     word [es:N_COUNT]
        mov     es,ax
        mov     [es:N_NEXT],dx
        pop     es
        pop     dx
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
        pop     es
        call    link_child
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
        mov     ax,[ss:di+20h]
        mov     [ent_size],ax
        mov     ax,[ss:di+22h]
        mov     [ent_size+2],ax
        mov     ax,[ss:di+1Ch]
        mov     [ent_size+4],ax
        mov     ax,[ss:di+1Eh]
        mov     [ent_size+6],ax
        lea     si,[di+2Ch]
        cmp     byte [ss:si],0
        jne     .name
        lea     si,[di+130h]            ; no long name: the short one
        jmp     .name
.s:     mov     al,[ss:di+15h]
        mov     [ent_attr],al
        mov     ax,[ss:di+1Ah]
        mov     [ent_size],ax
        mov     ax,[ss:di+1Ch]
        mov     [ent_size+2],ax
        mov     word [ent_size+4],0
        mov     word [ent_size+6],0
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
        mov     word [pathbuf+bx],'*.'
        mov     word [pathbuf+bx+2],'*'
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
        mov     di,add_app
        xor     ax,ax
        mov     cx,10                   ; add_app, add_disk: 0; add_desc: 1
        rep     stosw
        mov     word [add_desc],1
        test    [ent_attr],10h
        jnz     .sized
        mov     si,ent_size
        mov     di,add_app
        mov     cx,4
        rep     movsw
        call    round_to_cluster
.sized:
        push    es
        mov     es,[bp-S_TARGET]
        test    byte [es:N_FLAGS],F_PSEUDO
        pop     es
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
        mov     ax,[bp-S_CHILD]
        call    link_child
        mov     si,add_app              ; its own sizes
        xor     di,di
        mov     cx,8
        rep     movsw
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
        push    es
        mov     es,[pendseg]
        mov     ax,[es:si]
        mov     cl,[es:si+2]
        xor     ch,ch
        add     si,3
        mov     bx,[bp-S_BASE]
        lea     di,[pathbuf+bx]
        add     bx,cx
        mov     [pathlen],bx
        jcxz    .z
@@:     mov     dl,[es:si]
        mov     [di],dl
        inc     si
        inc     di
        loop    @b
.z:     mov     byte [di],0
        pop     es
        ret

; now and then: the keyboard (q or Esc stops), and the treemap of what is found so far
progress:
        inc     word [seen]
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
        mov     dx,ax
        sub     dx,[last_tick]
        cmp     dx,4                    ; about 0.2 s
        jb      .r
        mov     [last_tick],ax
        call    live_render
.r:     ret
.abort:
        mov     [abort],1
        ret

; ax ← the BIOS tick count's low word (18.2 a second): enough for the gaps measured here
ticks:
        push    es
        push    40h
        pop     es
        mov     ax,[es:6Ch]
        pop     es
        ret

live_render:
        pusha
        push    es
        push    ds
        pop     es
        inc     [spin]
        mov     ax,[root]
        mov     [cur],ax
        mov     [zoom],0
        mov     [sel],-1
        call    change_files
        call    render
        pop     es
        popa
        ret

; ---------------------------------------------------------------------------------------------
; the listing: the folder's entries, largest first (files_in_folder)

build_listing:
        push    es
        mov     es,[cur]
        mov     ax,[es:N_CHILD]
        xor     bx,bx
.l:     test    ax,ax
        jz      .e
        cmp     bx,MAXENT*2
        jae     .e
        mov     es,[entseg]
        mov     [es:bx],ax
        add     bx,2
        mov     es,ax
        mov     ax,[es:N_NEXT]
        jmp     .l
.e:     shr     bx,1
        mov     [nent],bx
        ; Shell sort, in the listing's segment
        mov     es,[entseg]
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
        mov     ax,[es:bx]
        mov     [tmp_e],ax
        mov     di,cx
.j:     cmp     di,[gap]
        jb      .put
        mov     bx,di
        sub     bx,[gap]
        shl     bx,1
        mov     dx,[es:bx]
        mov     ax,[tmp_e]
        call    before
        jnc     .put
        mov     bx,di
        shl     bx,1
        mov     [es:bx],dx
        sub     di,[gap]
        jmp     .j
.put:   mov     bx,di
        shl     bx,1
        mov     ax,[tmp_e]
        mov     [es:bx],ax
        inc     cx
        jmp     .i
.done:  pop     es
        ret

; CF=1 if node ax comes before node dx: larger first, then by name
before:
        push    ax
        push    bx
        push    si
        push    di
        push    es
        mov     si,[size_off]
        mov     es,ax
        mov     bx,[es:si]
        mov     [bf_a],bx
        mov     bx,[es:si+2]
        mov     [bf_a+2],bx
        mov     bx,[es:si+4]
        mov     [bf_a+4],bx
        mov     bx,[es:si+6]
        mov     [bf_a+6],bx
        mov     es,dx
        mov     bx,6
@@:     mov     di,[bf_a+bx]
        cmp     di,[es:si+bx]
        jne     .size
        sub     bx,2
        jnc     @b
        push    ds                      ; the same size: by name, a in DS, b in ES
        mov     ds,ax
        mov     si,N_NAME
.n:     mov     al,[si]
        cmp     al,[es:si]
        jne     .named                  ; CF=1 if a's byte is lower
        test    al,al
        jz      .same
        inc     si
        jmp     .n
.same:  pop     ds
        clc
        jmp     .out
.named: pop     ds
        jmp     .out
.size:  ja      .yes
        clc
        jmp     .out
.yes:   stc
.out:   pop     es
        pop     di
        pop     si
        pop     bx
        pop     ax
        ret

; bx = listing index → ax = its node
ent_node:
        push    es
        push    bx
        mov     es,[entseg]
        shl     bx,1
        mov     ax,[es:bx]
        pop     bx
        pop     es
        ret

; bx = listing index → ld_size: its size (the kind shown), 64 bits; si = ld_size
load_size:
        push    ax
        push    di
        push    es
        call    ent_node
        mov     es,ax
        mov     di,[size_off]
        mov     ax,[es:di]
        mov     [ld_size],ax
        mov     ax,[es:di+2]
        mov     [ld_size+2],ax
        mov     ax,[es:di+4]
        mov     [ld_size+4],ax
        mov     ax,[es:di+6]
        mov     [ld_size+6],ax
        pop     es
        pop     di
        pop     ax
        mov     si,ld_size
        ret

; ---------------------------------------------------------------------------------------------
; the treemap (treemap.rs), in doubles computed in software, in Rust's order: each value here is
; the f64 the Rust code has at the same point

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

; the empty space: the board; the total area w × h
tm_init:
        mov     ax,[bnd_x]
        mov     bx,em_x
        call    f_from_u16
        mov     ax,[bnd_y]
        mov     bx,em_y
        call    f_from_u16
        mov     ax,[bnd_w]
        mov     bx,em_w
        call    f_from_u16
        mov     ax,[bnd_h]
        mov     bx,em_h
        call    f_from_u16
        mov     si,em_w
        mov     di,em_h
        mov     bx,tot_area
        call    f_mul
        mov     [ntiles],0
        mov     [has_small],0
        ret

; the total the percentages are of: the folder's size, or what its entries add up to if more,
; less what the zoom leaves off; in integers, as files_in_folder has it
setup_files:
        mov     ax,[zoom]
        cmp     ax,[nent]
        jbe     @f
        mov     ax,[nent]
        mov     [zoom],ax
@@:     mov     cx,[nent]
        sub     cx,ax
        mov     [nfiles],cx
        mov     di,sum64
        xor     ax,ax
        mov     cx,4
        rep     stosw
        xor     bx,bx
.sum:   cmp     bx,[nent]
        jae     .summed
        call    load_size
        mov     di,sum64
        call    add64
        inc     bx
        jmp     .sum
.summed:
        push    es                      ; the folder's own, if bigger
        mov     es,[cur]
        mov     si,[size_off]
        mov     bx,6
@@:     mov     ax,[es:si+bx]
        cmp     ax,[sum64+bx]
        jne     @f
        sub     bx,2
        jnc     @b
        jmp     .keep
@@:     jb      .keep
        mov     bx,0
@@:     mov     ax,[es:si+bx]
        mov     [sum64+bx],ax
        add     bx,2
        cmp     bx,8
        jb      @b
.keep:  pop     es
        mov     si,sum64                ; unzoomed, for the side panel and the status line
        mov     bx,ftotal0
        call    f_from_u64
        xor     bx,bx
.drop:  cmp     bx,[zoom]
        jae     .dropped
        call    load_size
        mov     di,sum64
        call    sub64
        inc     bx
        jmp     .drop
.dropped:
        mov     si,sum64
        mov     bx,ftotal
        call    f_from_u64
        mov     [area_cached],0
        mov     [equal],0
        cmp     byte [ftotal+F_C],FC_ZERO
        jne     .r
        mov     [equal],1               ; nothing has a size: equal shares, 1 / n
        cmp     [nfiles],0
        je      .r
        mov     ax,[nfiles]
        mov     bx,f_t1
        call    f_from_u16
        mov     si,fc_one
        mov     di,f_t1
        mov     bx,eq_pct
        call    f_div
.r:     ret

; [di] += [si], 64 bits
add64:
        mov     ax,[si]
        add     [di],ax
        mov     ax,[si+2]
        adc     [di+2],ax
        mov     ax,[si+4]
        adc     [di+4],ax
        mov     ax,[si+6]
        adc     [di+6],ax
        ret

; [di] -= [si], 64 bits
sub64:
        mov     ax,[si]
        sub     [di],ax
        mov     ax,[si+2]
        sbb     [di+2],ax
        mov     ax,[si+4]
        sbb     [di+4],ax
        mov     ax,[si+6]
        sbb     [di+6],ax
        ret

; bx = listing index → f_pct: its share of the folder (1.0 is all of it), after the zoom
pct_of:
        push    si
        push    di
        push    bx
        cmp     [equal],0
        jne     .eq
        call    load_size
        mov     bx,f_pct
        call    f_from_u64
        mov     si,f_pct
        mov     di,ftotal
        call    f_div
        jmp     .out
.eq:    mov     si,eq_pct
        mov     bx,f_pct
        call    f_copy
.out:   pop     bx
        pop     di
        pop     si
        ret

; bx = listing index → f_pct: its share of the whole folder, whatever the zoom
pct0_of:
        push    si
        push    di
        push    bx
        cmp     [nent],0
        je      .none
        cmp     byte [ftotal0+F_C],FC_ZERO
        je      .eq
        call    load_size
        mov     bx,f_pct
        call    f_from_u64
        mov     si,f_pct
        mov     di,ftotal0
        call    f_div
        jmp     .out
.eq:    mov     ax,[nent]
        mov     bx,f_t1
        call    f_from_u16
        mov     si,fc_one
        mov     di,f_t1
        mov     bx,f_pct
        call    f_div
        jmp     .out
.none:  mov     bx,f_pct
        xor     ax,ax
        call    f_from_u16
.out:   pop     bx
        pop     di
        pop     si
        ret

; bx = listing index → f_area: the area it gets on the board (percentage × total_size). Sizes
; come largest first, often many alike: the last one's answer is kept.
area_of:
        push    si
        push    di
        push    bx
        cmp     [equal],0
        jne     .compute                ; all the same share, and cheap
        call    load_size
        cmp     [area_cached],0
        je      .compute
        mov     ax,[ld_size]
        cmp     ax,[area_size]
        jne     .compute
        mov     ax,[ld_size+2]
        cmp     ax,[area_size+2]
        jne     .compute
        mov     ax,[ld_size+4]
        cmp     ax,[area_size+4]
        jne     .compute
        mov     ax,[ld_size+6]
        cmp     ax,[area_size+6]
        je      .out
.compute:
        pop     bx
        push    bx
        call    pct_of
        mov     si,f_pct
        mov     di,tot_area
        mov     bx,f_area
        call    f_mul
        mov     si,ld_size
        mov     di,area_size
        push    es
        push    ds
        pop     es
        mov     cx,4
        rep     movsw
        pop     es
        mov     [area_cached],1
.out:   pop     bx
        pop     di
        pop     si
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
        mov     si,f_area
        mov     di,fc_min_prod
        call    f_cmp                   ; renderable if min1 × min2 <= its area
        cmp     al,F_GREATER
        je      .renderable
        cmp     al,F_EQUAL
        je      .renderable
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
        mov     si,w_worst
        mov     bx,w_cur
        call    f_copy
        mov     ax,[rs]
        mov     dx,[ri]
        inc     dx
        call    worst
        jc      .lay
        mov     si,w_cur
        mov     di,w_worst
        call    f_cmp
        cmp     al,F_LESS
        je      .add
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
        push    es
        push    ds
        pop     es
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
.kept:  pop     es
        mov     [ntiles],dx
.r:     ret

; the row's length and the least sides a tile may have, by which side of the space is shorter
compute_len:
        mov     si,em_h
        mov     di,fc_ratio
        mov     bx,f_t1
        call    f_mul                   ; height × 2.5
        mov     si,f_t1
        mov     di,em_w
        call    f_cmp
        cmp     al,F_LESS
        jne     .narrow
        mov     si,f_t1
        mov     bx,f_len
        call    f_copy
        mov     si,fc_min_hr            ; 3 × 2.5
        mov     bx,min1
        call    f_copy
        mov     si,fc_min_wr            ; 8 / 2.5
        mov     bx,min2
        call    f_copy
        ret
.narrow:
        mov     si,em_w
        mov     di,fc_ratio
        mov     bx,f_len
        call    f_div
        mov     si,fc_min_wr
        mov     bx,min1
        call    f_copy
        mov     si,fc_min_hr
        mov     bx,min2
        call    f_copy
        ret

; the worst aspect ratio of the row [ax, dx) → w_worst, or CF=1 if the row is empty or has an
; entry that cannot be drawn (worst_in_renderable_row)
worst:
        cmp     ax,dx
        jae     .none
        mov     [w_a],ax
        mov     [w_b],dx
        mov     bx,w_sum
        xor     ax,ax
        call    f_from_u16
        mov     bx,[w_a]
.s:     call    area_of
        mov     si,w_sum
        mov     di,f_area
        push    bx
        mov     bx,w_sum
        call    f_add
        pop     bx
        inc     bx
        cmp     bx,[w_b]
        jb      .s
        mov     [w_have],0
        mov     bx,[w_a]
.l:     call    area_of
        push    bx
        mov     si,f_area               ; first = size / sum × length
        mov     di,w_sum
        mov     bx,w_first
        call    f_div
        mov     si,w_first
        mov     di,f_len
        call    f_mul
        mov     si,f_area               ; second = size / first
        mov     di,w_first
        mov     bx,w_second
        call    f_div
        pop     bx
        mov     si,w_first
        mov     di,min1
        call    f_cmp
        cmp     al,F_GREATER
        je      @f
        cmp     al,F_EQUAL
        jne     .none
@@:     mov     si,w_second
        mov     di,min2
        call    f_cmp
        cmp     al,F_GREATER
        je      @f
        cmp     al,F_EQUAL
        jne     .none
@@:     push    bx
        mov     si,w_first
        mov     di,w_second
        call    f_cmp
        cmp     al,F_LESS
        jne     @f
        mov     bx,f_t1                 ; first < second: first / second
        call    f_div
        jmp     .ratio
@@:     mov     si,w_second
        mov     di,w_first
        mov     bx,f_t1
        call    f_div
.ratio: pop     bx
        cmp     [w_have],0
        je      .take
        mov     si,f_t1
        mov     di,w_worst
        call    f_cmp
        cmp     al,F_LESS
        jne     .nx
.take:  push    bx
        mov     si,f_t1
        mov     bx,w_worst
        call    f_copy
        pop     bx
        mov     [w_have],1
.nx:    inc     bx
        cmp     bx,[w_b]
        jb      .l
        clc
        ret
.none:  stc
        ret

; lay out the row [ax, dx) along the empty space's shorter side (layoutrow)
layoutrow:
        cmp     ax,dx
        jae     .ret
        mov     [l_a],ax
        mov     [l_b],dx
        mov     bx,l_total
        xor     ax,ax
        call    f_from_u16
        mov     bx,[l_a]
.s:     call    area_of
        push    bx
        mov     si,l_total
        mov     di,f_area
        mov     bx,l_total
        call    f_add
        pop     bx
        inc     bx
        cmp     bx,[l_b]
        jb      .s
        ; horizontal if width <= height × 2.5
        mov     [l_horiz],0
        mov     si,em_h
        mov     di,fc_ratio
        mov     bx,f_t1
        call    f_mul
        mov     si,em_w
        mov     di,f_t1
        call    f_cmp
        cmp     al,F_LESS
        je      @f
        cmp     al,F_EQUAL
        jne     .dir
@@:     mov     [l_horiz],1
.dir:   mov     si,em_y
        cmp     [l_horiz],0
        je      @f
        mov     si,em_x
@@:     mov     bx,l_prog
        call    f_copy
        mov     bx,l_second
        xor     ax,ax
        call    f_from_u16
        mov     bx,[l_a]
.l:     call    area_of
        push    bx
        mov     si,f_area               ; first = size / total × (width or height)
        mov     di,l_total
        mov     bx,l_first
        call    f_div
        mov     di,em_h
        cmp     [l_horiz],0
        je      @f
        mov     di,em_w
@@:     mov     si,l_first
        call    f_mul
        mov     si,f_area               ; the candidate for the other side
        mov     di,l_first
        mov     bx,f_t2
        call    f_div
        mov     si,l_second             ; the larger of it and the row's so far
        mov     di,f_t2
        call    f_cmp
        cmp     al,F_GREATER
        je      @f
        mov     si,f_t2
@@:     mov     bx,l_tsecond
        call    f_copy
        cmp     [l_horiz],0
        je      .vr
        mov     si,l_prog
        mov     bx,r_x
        call    f_copy
        mov     si,em_y
        mov     bx,r_y
        call    f_copy
        mov     si,l_first
        mov     bx,r_w
        call    f_copy
        mov     si,l_tsecond
        mov     bx,r_h
        call    f_copy
        jmp     .rr
.vr:    mov     si,em_x
        mov     bx,r_x
        call    f_copy
        mov     si,l_prog
        mov     bx,r_y
        call    f_copy
        mov     si,l_tsecond
        mov     bx,r_w
        call    f_copy
        mov     si,l_first
        mov     bx,r_h
        call    f_copy
.rr:    mov     si,l_prog
        mov     di,l_first
        mov     bx,l_prog
        call    f_add
        call    round_rect
        pop     bx
        cmp     [t_h],MINIMUM_HEIGHT
        jb      .small
        cmp     [t_w],MINIMUM_WIDTH
        jb      .small
        call    push_tile
        jmp     .upd
.small: call    add_unrenderable
.upd:   mov     si,l_tsecond
        mov     di,l_second
        call    f_cmp
        cmp     al,F_GREATER
        jne     .nx
        push    bx
        mov     si,l_tsecond
        mov     bx,l_second
        call    f_copy
        pop     bx
.nx:    inc     bx
        cmp     bx,[l_b]
        jb      .l
        cmp     [l_horiz],0
        je      .vs
        mov     si,em_h
        mov     di,l_second
        mov     bx,em_h
        call    f_sub
        mov     si,em_y
        mov     bx,em_y
        call    f_add
        ret
.vs:    mov     si,em_w
        mov     di,l_second
        mov     bx,em_w
        call    f_sub
        mov     si,em_x
        mov     bx,em_x
        call    f_add
.ret:   ret

; r_x..r_h → t_x..t_h (RectFloat::round)
round_rect:
        push    bx
        mov     si,r_x
        call    f_round_u16
        mov     [t_x],ax
        mov     si,r_y
        call    f_round_u16
        mov     [t_y],ax
        mov     si,r_x                  ; w = ((x - rounded x) + width).round()
        mov     ax,[t_x]
        mov     di,r_w
        call    .side
        mov     [t_w],ax
        mov     si,r_y
        mov     ax,[t_y]
        mov     di,r_h
        call    .side
        mov     [t_h],ax
        mov     si,r_x                  ; one more if (x + width).round() is further
        mov     di,r_w
        mov     bx,f_t1
        call    f_add
        mov     si,f_t1
        call    f_round_u16
        mov     dx,[t_x]
        add     dx,[t_w]
        cmp     ax,dx
        jbe     @f
        inc     [t_w]
@@:     mov     si,r_y
        mov     di,r_h
        mov     bx,f_t1
        call    f_add
        mov     si,f_t1
        call    f_round_u16
        mov     dx,[t_y]
        add     dx,[t_h]
        cmp     ax,dx
        jbe     @f
        inc     [t_h]
@@:     pop     bx
        ret
; si = the start, ax = it rounded, di = the length → ax = ((start - rounded) + length).round()
.side:  mov     bx,f_t2
        call    f_from_u16
        push    di
        mov     di,f_t2
        mov     bx,f_t2
        call    f_sub
        pop     di
        mov     si,f_t2
        mov     bx,f_t2
        call    f_add
        mov     si,f_t2
        jmp     f_round_u16

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
        call    ent_node
        mov     [di+T_NODE],ax
        mov     [di+T_IDX],bx
        inc     [ntiles]
.r:     ret

; the treemap's constants, as doubles
f_constants:
        mov     ax,1
        mov     bx,fc_one
        call    f_from_u16
        mov     ax,3
        mov     bx,f_t1
        call    f_from_u16
        mov     ax,8
        mov     bx,f_t2
        call    f_from_u16
        mov     ax,5
        mov     bx,fc_ratio
        call    f_from_u16
        mov     ax,2
        mov     bx,f_pct
        call    f_from_u16
        mov     si,fc_ratio             ; 2.5 = 5 / 2
        mov     di,f_pct
        mov     bx,fc_ratio
        call    f_div
        mov     si,f_t1                 ; MINIMUM_HEIGHT × 2.5
        mov     di,fc_ratio
        mov     bx,fc_min_hr
        call    f_mul
        mov     si,f_t2                 ; MINIMUM_WIDTH / 2.5
        mov     di,fc_ratio
        mov     bx,fc_min_wr
        call    f_div
        mov     si,fc_min_hr            ; the least area a tile may have, either way round
        mov     di,fc_min_wr
        mov     bx,fc_min_prod
        call    f_mul
        mov     ax,100
        mov     bx,fc_100
        call    f_from_u16
        mov     si,k_g                  ; DisplaySize's thresholds
        mov     bx,fc_g
        call    f_from_u64
        mov     si,k_m
        mov     bx,fc_m
        call    f_from_u64
        mov     si,k_k
        mov     bx,fc_k
        call    f_from_u64
        ret

; ---------------------------------------------------------------------------------------------
; drawing, into `scr`, then onto the screen at once

render:
        push    ds
        pop     es
        mov     di,scr
        mov     ax,0720h
        mov     cx,80*25
        rep     stosw
        call    sync_board
        call    pv_target
        call    draw_title
        call    draw_panel
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
        xor     al,al                   ; the picture's colours while it is on screen
        cmp     [panel_on],0
        je      @f
        cmp     [pv_state],PV_PICTURE
        jne     @f
        mov     al,1
        cmp     [pal_want],0            ; a new picture: its colours, even if one was up
        je      @f
        mov     [pal_want],0
        mov     [pal_applied],0
@@:     call    pal_set
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

; the file in hand, to preview: a file, not a folder or "(not in memory)", with the panel shown
; and the scan done
pv_target:
        xor     ax,ax
        cmp     [panel_on],0
        je      .set
        cmp     [scanning],0
        jne     .set
        call    selected_entry
        cmp     ax,-1
        je      .none
        mov     bx,ax
        call    ent_node
        push    ax
        call    nflags
        test    al,F_DIR+F_PSEUDO
        pop     ax
        jz      .set
.none:  xor     ax,ax
.set:   jmp     pv_update

; ax = a node → al = its flags; the rest kept
nflags:
        push    es
        mov     es,ax
        mov     al,[es:N_FLAGS]
        pop     es
        ret

; ax = a node → dx:ax = its descendants
ndesc:
        push    es
        mov     es,ax
        mov     ax,[es:N_DESC]
        mov     dx,[es:N_DESC+2]
        pop     es
        ret

; ax = a node → its name at ds:di, di past it, cx its length; ES kept
nname:
        push    si
        push    es
        mov     es,ax
        mov     cl,[es:N_NLEN]
        xor     ch,ch
        push    cx
        mov     si,N_NAME
        jcxz    .d
@@:     mov     al,[es:si]
        mov     [di],al
        inc     si
        inc     di
        loop    @b
.d:     pop     cx
        mov     ax,es
        pop     es
        pop     si
        ret

; ax = a node → f_sz: its size (the kind shown) as a double; ld_size the integer
nsize:
        pusha
        push    es
        mov     es,ax
        mov     si,[size_off]
        mov     di,ld_size
        mov     cx,4
@@:     mov     ax,[es:si]
        mov     [di],ax
        add     si,2
        add     di,2
        loop    @b
        pop     es
        mov     si,ld_size
        mov     bx,f_sz
        call    f_from_u64
        popa
        ret

; dl = x, dh = y, al = character, ah = attribute
put_ch:
        cmp     dl,80
        jae     .r
        cmp     dh,25
        jae     .r
        push    bx
        push    dx
        mov     bl,dh
        xor     bh,bh
        imul    bx,bx,80
        xor     dh,dh
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
        mov     bl,dh
        xor     bh,bh
        imul    bx,bx,80
        xor     dh,dh
        add     bx,dx
        shl     bx,1
        mov     dl,byte [scr+bx]
        xor     dh,dh
        xchg    bx,dx
        or      al,[boxbits+bx]
        mov     bl,al
        xor     bh,bh
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
        mov     ax,[root]
        call    nsize
        mov     si,f_sz
        call    fmt_size
        mov     si,s_lparen
        call    append_z
        mov     ax,[root]
        call    ndesc
        call    u32dec_c
        mov     si,s_files_close
        call    append_z
        cmp     [scanning],0
        jne     .spin
        mov     si,s_freed
        call    append_z
        mov     si,[size_off]
        add     si,freed
        mov     bx,f_sz
        call    f_from_u64
        mov     si,f_sz
        call    fmt_size
        jmp     .left
.spin:  mov     al,' '
        stosb
        mov     bl,[spin]
        xor     bh,bh
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
        mov     si,s_lparen
        call    append_z                ; lodsb: AL is not safe across it
        mov     ax,[cur]
        call    nsize
        mov     si,f_sz
        call    fmt_size
        mov     si,s_comma
        call    append_z
        mov     ax,[cur]
        call    ndesc
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
        call    selected_entry
        push    ds
        pop     es
        mov     di,tb
        cmp     ax,-1
        je      .none
        mov     bx,ax
        call    ent_node
        mov     [st_node],ax
        push    bx
        mov     si,s_selected
        call    append_z
        mov     ax,[st_node]
        call    nname
        mov     ax,[st_node]
        call    nflags
        mov     [st_flags],al
        test    al,F_DIR
        jz      @f
        mov     al,'/'
        stosb
@@:     mov     si,s_sep
        call    append_z
        mov     ax,[st_node]
        call    nsize
        mov     si,f_sz
        call    fmt_size
        mov     si,s_lparen
        call    append_z
        pop     bx
        mov     [st_di],di
        call    pct0_of
        mov     si,f_pct
        mov     di,fc_100
        mov     bx,f_t1
        call    f_mul
        mov     si,f_t1
        mov     cx,1
        call    f_round_even
        mov     di,[st_di]
        call    u32dec
        mov     si,s_pct_close
        call    append_z
        test    [st_flags],F_DIR
        jz      .show
        mov     si,s_sep
        call    append_z
        mov     ax,[st_node]
        call    ndesc
        call    u32dec_c
        mov     si,s_files
        call    append_z
        test    [st_flags],F_SKIPPED
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
        mov     ax,[zoom]
        xor     dx,dx
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
        mov     dl,0
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
        mov     ax,[bnd_w]              ; centred in the board
        sub     ax,15-1
        shr     ax,1
        add     ax,[bnd_x]
        mov     dl,al
        mov     ax,[bnd_h]
        shr     ax,1
        add     ax,[bnd_y]
        dec     ax
        mov     dh,al
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
        mov     ax,[si+T_NODE]
        mov     [dt_node],ax
        call    nflags
        mov     [dt_flags],al
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
        test    [dt_flags],F_DIR
        jz      @f
        mov     [a1],A_DIR
@@:     cmp     ax,[sel]
        jne     .text
        mov     [a1],70h
        mov     [a2],70h
        mov     bx,7700h+0DBh
        test    [dt_flags],F_DIR
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

; dt_node → l1buf, l1len: its name, a folder's with "/" and its descendants if they fit
build_line1:
        mov     di,l1buf
        mov     ax,[dt_node]
        call    nname
        test    [dt_flags],F_DIR
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
        mov     ax,[dt_node]
        call    ndesc
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
        call    pct_of                  ; percentage × 100
        mov     si,f_pct
        mov     di,fc_100
        mov     bx,f_t1
        call    f_mul
        mov     si,f_t1
        mov     cx,1
        call    f_round_even            ; "{:.0}%"
        mov     [pctv_even],ax
        call    f_round_u16             ; "{}%" of .round()
        mov     [pctv],ax
        pop     bx
        call    load_size
        mov     bx,f_sz
        call    f_from_u64
        mov     di,s1buf
        mov     si,f_sz
        call    fmt_size
        mov     ax,di
        sub     ax,s1buf
        mov     [s1len],ax
        mov     di,s2buf
        mov     si,f_sz
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
        mov     ax,[pctv_even]
        xor     dx,dx
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
        mov     ax,[pctv]
        xor     dx,dx
        call    u32dec
        mov     si,s_pct_close
        call    append_z
        jmp     .done
.e:     mov     ax,[pctv]
        xor     dx,dx
        call    u32dec
        mov     al,'%'
        stosb
.done:  mov     ax,di
        sub     ax,l2buf
        mov     [l2len],ax
        ret

; ---------------------------------------------------------------------------------------------
; numbers

; dx:ax ← dx:ax / 10, bx ← the remainder
div10:
        push    cx
        mov     bx,10
        mov     cx,ax
        mov     ax,dx
        xor     dx,dx
        div     bx
        xchg    ax,cx                   ; cx: the high word of the quotient
        div     bx
        mov     bx,dx
        mov     dx,cx
        pop     cx
        ret

; dx:ax → es:di in decimal
u32dec:
        push    ax
        push    bx
        push    cx
        push    dx
        xor     cx,cx
.l:     call    div10
        push    bx
        inc     cx
        mov     bx,ax
        or      bx,dx
        jnz     .l
.o:     pop     ax
        add     al,'0'
        stosb
        loop    .o
        pop     dx
        pop     cx
        pop     bx
        pop     ax
        ret

; dx:ax → es:di with thousands separated (DisplayCount)
u32dec_c:
        push    ax
        push    bx
        push    cx
        push    dx
        xor     cx,cx
.l:     call    div10
        push    bx
        inc     cx
        mov     bx,ax
        or      bx,dx
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
.d:     pop     dx
        pop     cx
        pop     bx
        pop     ax
        ret

; the double at [si], bytes → es:di, e.g. "5.2G" (DisplaySize): {:.1} of it over its unit
fmt_size:
        push    ax
        push    bx
        push    cx
        push    dx
        push    si
        call    size_unit
        jc      .plain
        mov     si,f_unit
        mov     cx,10
        call    f_round_even            ; tenths, exactly as {:.1} rounds
        push    bx
        call    div10
        call    u32dec
        mov     al,'.'
        stosb
        mov     al,bl
        add     al,'0'
        stosb
        pop     bx
        mov     al,bl
        stosb
        jmp     .out
.plain: mov     cx,1
        call    f_round_even
        call    u32dec
.out:   pop     si
        pop     dx
        pop     cx
        pop     bx
        pop     ax
        ret

; the same without the decimal, e.g. "5G" (DisplaySizeRounded)
fmt_size_r:
        push    ax
        push    bx
        push    cx
        push    dx
        push    si
        call    size_unit
        jc      @f
        mov     si,f_unit
@@:     pushf
        mov     cx,1
        call    f_round_even
        call    u32dec
        popf
        jc      @f
        mov     al,bl
        stosb
@@:     pop     si
        pop     dx
        pop     cx
        pop     bx
        pop     ax
        ret

; [si], bytes → f_unit, in its unit, and bl the unit's letter; CF=1 for plain bytes. Dividing
; by 2^30, 2^20 or 2^10 is exact: the exponent moves.
size_unit:
        push    di
        mov     di,fc_g
        mov     bx,30*256+'G'
        call    f_cmp
        cmp     al,F_GREATER
        je      .unit
        mov     di,fc_m
        mov     bx,20*256+'M'
        call    f_cmp
        cmp     al,F_GREATER
        je      .unit
        mov     di,fc_k
        mov     bx,10*256+'K'
        call    f_cmp
        cmp     al,F_GREATER
        je      .unit
        pop     di
        stc
        ret
.unit:  push    bx
        mov     bx,f_unit
        call    f_copy
        pop     bx
        mov     al,bh
        xor     ah,ah
        sub     word [f_unit+F_E],ax
        pop     di
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
        push    es
        mov     [np_start],di
        mov     [np_cut],0
        xor     bx,bx
.up:    mov     [chain+bx],ax
        add     bx,2
        mov     es,ax
        mov     ax,[es:N_PARENT]
        test    ax,ax
        jz      .down
        cmp     bx,2*64
        jb      .up
        mov     [np_cut],1              ; deeper than any walk goes
.down:  sub     bx,2
        mov     es,[chain+bx]
        cmp     di,[np_start]
        je      .name
        cmp     byte [di-1],'\'
        je      .name
        mov     ax,di
        sub     ax,[np_start]
        cmp     ax,NP_MAX
        jae     .cut
        mov     byte [di],'\'
        inc     di
.name:  mov     cl,[es:N_NLEN]
        xor     ch,ch
        mov     si,N_NAME
        cmp     [np_disk],0
        je      .fit
        test    byte [es:N_FLAGS],F_SHORT
        jz      .fit
        add     si,cx                   ; the short name, after the long one's NUL
        inc     si
        push    si
        xor     cx,cx
@@:     cmp     byte [es:si],0
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
@@:     mov     al,[es:si]
        mov     [di],al
        inc     si
        inc     di
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
        pop     es
        pop     si
        pop     cx
        pop     bx
        pop     ax
        ret

; ---------------------------------------------------------------------------------------------
; input

; al = character, ah = scan code. CF=1 to quit.
handle_key:
        cmp     al,9
        je      .tab
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
        cmp     al,'s'
        je      .panel
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
        mov     dx,-(LIST_ROWS-1)
        cmp     ah,49h                  ; Page Up, Page Down, Home, End: through the list
        je      .jump
        neg     dx
        cmp     ah,51h
        je      .jump
        mov     dx,-7FFFh
        cmp     ah,47h
        je      .jump
        mov     dx,7FFFh
        cmp     ah,4Fh
        je      .jump
        clc
        ret
.jump:  call    list_has_keys
        jnz     .r
        mov     [cursor_chosen],1
        mov     ax,dx
        call    move_list
        clc
        ret
.tab:   cmp     [focus],0               ; the other panel
        jne     @f
        call    focus_treemap
        clc
        ret
@@:     call    focus_list
        clc
        ret
.panel: xor     [panel_on],1            ; the side panel, shown or not; the list keeps the
        call    panel_layout            ; keyboard underneath, for when it comes back
        call    fill
        clc
        ret
.quit:  stc
        ret
.enter: call    list_has_keys
        jnz     @f
        call    list_pos
        call    enter_idx
        clc
        ret
@@:     call    enter_sel
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
.right: mov     [cursor_chosen],1
        call    list_has_keys           ; right from the list goes back to the treemap
        jnz     @f
        call    focus_treemap
        clc
        ret
@@:     mov     al,0
        jmp     .move
.left:  mov     [cursor_chosen],1
        call    list_has_keys
        jz      .r
        mov     bx,[sel]                ; off the treemap's left edge: into the list
        push    bx
        mov     al,1
        call    move_sel
        pop     bx
        cmp     bx,-1
        je      .r
        cmp     [sel],-1
        jne     .r
        cmp     [panel_on],0
        je      .r
        mov     [sel],bx
        call    focus_list
        clc
        ret
.down:  mov     ax,1
        mov     cl,2
        jmp     .vert
.upk:   mov     ax,-1
        mov     cl,3
.vert:  call    list_has_keys
        jnz     @f
        mov     [cursor_chosen],1
        call    move_list
        clc
        ret
@@:     mov     al,cl
.move:  call    move_sel
.r:     clc
        ret

; ZF=1 if the list has the keyboard
list_has_keys:
        cmp     [panel_on],0
        je      .no
        cmp     [focus],0
        ret
.no:    or      sp,sp                   ; ZF=0
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

; Enter on the treemap: the selected tile's folder, or the largest folder when nothing is
; selected (handle_enter)
enter_sel:
        cmp     [sel],-1
        jne     .have
        xor     cx,cx                   ; Board::move_to_largest_folder
        mov     si,tiles
.l:     cmp     cx,[ntiles]
        jae     .r
        mov     ax,[si+T_NODE]
        call    nflags
        test    al,F_DIR
        jnz     .found
        inc     cx
        add     si,T_SIZE
        jmp     .l
.found: mov     [sel],cx
.have:  mov     si,[sel]
        shl     si,4
        mov     ax,word [tiles+si+T_IDX]
        jmp     enter_idx
.r:     ret

; ax = a listing index: into it, if it is a folder (enter_named)
enter_idx:
        cmp     ax,[nent]
        jae     .r
        mov     bx,ax
        call    ent_node
        push    ax
        call    nflags
        test    al,F_DIR
        pop     ax
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
        mov     [list_cur],0
        mov     [cursor_chosen],0
        call    change_files
        call    list_has_keys           ; arriving with the list in hand: its top
        jnz     .r
        cmp     [nent],0
        je      .r
        xor     ax,ax
        call    set_list_cursor
.r:     ret

go_up:
        mov     ax,[cur]
        cmp     ax,[root]
        je      .r
        mov     [left_node],ax
        push    es
        mov     es,ax
        mov     ax,[es:N_PARENT]
        pop     es
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
        mov     [list_cur],0
        call    change_files
        call    list_has_keys           ; back in the parent, the list is on the folder left
        jnz     .r
        mov     ax,[left_node]
        mov     [list_cur],ax
        call    list_cursor_index
        cmp     ax,-1
        je      .r
        call    set_list_cursor
.r:     ret

; a press of the left button selects the entry under it, in the list or the treemap, and gives
; that panel the keyboard; a second press on the same entry within half a second enters it
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
        push    cx
        push    dx
        call    entry_at
        pop     dx
        pop     cx
        cmp     ax,-1
        je      .tile
        mov     [focus],0
        push    ax
        call    set_list_cursor
        pop     ax
        jmp     .clicked
.tile:  call    tile_at
        cmp     ax,-1
        je      .no
        mov     [focus],1
        mov     [sel],ax
        mov     si,ax
        shl     si,4
        mov     ax,word [tiles+si+T_IDX]
.clicked:
        mov     [cursor_chosen],1
        cmp     [presses],2             ; both clicks of a double click since the last look
        jae     .enter
        push    ax
        call    ticks
        mov     dx,ax
        pop     ax
        cmp     ax,[click_idx]
        jne     .first
        mov     cx,dx
        sub     cx,[click_time]
        cmp     cx,9
        ja      .first
.enter: mov     [click_idx],-1
        call    enter_idx
        stc
        ret
.first: mov     [click_idx],ax
        mov     [click_time],dx
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
        call    selected_entry
        cmp     ax,-1
        je      .r
        mov     [del_idx],ax
        mov     [del_started],0
        mov     bx,ax
        call    ent_node
        mov     [del_node],ax
        call    nflags
        mov     [del_flags],al
        test    al,F_PSEUDO
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
        test    [del_flags],F_DIR
        jz      @f
        mov     si,s_del_dir
@@:     mov     dh,9
        call    dialog_line_z
        ; the name
        mov     di,l1buf
        mov     ax,[del_node]
        call    nname
        mov     si,l1buf
        mov     dx,54
        call    truncate_middle
        mov     si,l1buf
        mov     dh,11
        call    dialog_line
        ; its size
        mov     di,tb
        mov     ax,[del_node]
        call    nsize
        mov     si,f_sz
        call    fmt_size
        test    [del_flags],F_DIR
        jz      @f
        mov     si,s_lparen
        call    append_z
        mov     ax,[del_node]
        call    ndesc
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
        pushf                           ; its CF: the path was too long to write
        mov     cx,di
        sub     cx,pathbuf
        mov     [pathlen],cx
        popf
        mov     ax,3                    ; path not found
        jc      .failed
        cmp     cx,PATHMAX              ; longer than DOS takes
        jae     .failed
        mov     [rm_depth],0
        mov     [del_started],1
        test    [del_flags],F_DIR
        jz      .file
        call    rm_tree
        jmp     .did
.file:  call    rm_file
.did:   jc      .failed
        call    tree_remove
        mov     [cursor_chosen],0       ; its neighbour is in hand now, not picked
        call    change_files
        call    list_has_keys           ; the list's cursor on the entry that took its place
        jnz     .r2
        mov     ax,[nent]
        test    ax,ax
        jz      .r2
        dec     ax
        cmp     ax,[del_idx]
        jbe     @f
        mov     ax,[del_idx]
@@:     call    set_list_cursor
.r2:    ret
.failed:
        mov     [del_err],ax
        test    [del_flags],F_DIR       ; some of a folder may have gone: walk it again,
        jz      @f
        cmp     [del_started],0         ; unless it was refused before anything was tried
        je      @f
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
        mov     ax,[del_err]
        xor     dx,dx
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
        mov     word [pathbuf+bx],'*.'
        mov     word [pathbuf+bx+2],'*'
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
        push    es
        mov     es,[pendseg]
        mov     al,[es:si+2]
        pop     es
        xor     ah,ah
        push    ax
        add     ax,bx
        cmp     ax,PATHMAX
        pop     ax
        jb      @f
        add     si,ax                   ; skip it
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
; returned in ax (0 if the folder has gone or cannot be opened, and is taken out; the old node
; itself if there is no memory for a new one, and nothing changes). pathbuf = its path, as DOS needs it. With rs_freed
; set, what it lost since counts as freed. The old node is leaked, like everything taken out.
rescan_node:
        mov     [rs_old],ax
        push    es
        mov     es,ax
        mov     bx,[es:N_PARENT]
        pop     es
        mov     [rs_parent],bx
        test    bx,bx
        jz      .root
        call    probe_dir               ; still there, and can be opened?
        jc      .gone
        push    es                      ; a node like it: the name, its NUL, a short name
        mov     es,[rs_old]
        mov     cl,[es:N_NLEN]
        xor     ch,ch
        inc     cx
        test    byte [es:N_FLAGS],F_SHORT
        pop     es
        jz      @f
        add     cx,14
@@:     mov     [rs_bytes],cx
        dec     cx
        mov     al,1
        call    alloc_node
        jc      .keep                   ; no memory: the old one stays as it was
        mov     [rs_new],ax
        mov     cx,[rs_bytes]
        push    ds
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
        push    es
        mov     es,ax
        and     byte [es:N_FLAGS],F_DIR+F_SHORT
        pop     es
        mov     bx,[rs_parent]
        call    link_child
        mov     di,add_app              ; the folder above: one more entry
        xor     ax,ax
        mov     cx,10
        rep     stosw
        mov     word [add_desc],1
        mov     ax,bx
        call    add_up
        mov     ax,[rs_old]             ; now the old one out: freed += all of it, for now
        mov     [del_node],ax
        call    tree_remove
        jmp     .walk
.root:  mov     cx,[pathlen]            ; everything: a new root
        mov     al,1
        call    alloc_node
        jc      .keep
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
        push    es
@@:     mov     es,ax
        mov     ax,[es:N_PARENT]
        inc     cx
        test    ax,ax
        jnz     @b
        pop     es
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
@@:     mov     ax,[rs_new]             ; freed: what it lost, or nothing
        cmp     [rs_freed],0
        jne     @f
        mov     ax,[rs_old]
@@:     call    freed_less
.done:  mov     [rs_freed],0
        mov     ax,[rs_new]
        ret
.gone:  mov     ax,[rs_old]             ; gone from the disk: out of the tree too
        mov     [del_node],ax
        call    tree_remove
        cmp     [rs_freed],0            ; freed, if we deleted it
        jne     .fail
        mov     ax,[rs_old]
        call    freed_less
.fail:  mov     [rs_freed],0
        xor     ax,ax
        ret
.keep:  mov     [rs_freed],0
        mov     ax,[rs_old]
        ret

; freed less node ax's sizes
freed_less:
        push    bx
        push    es
        mov     es,ax
        xor     bx,bx
        mov     ax,[es:bx]
        sub     [freed],ax
        mov     ax,[es:bx+2]
        sbb     [freed+2],ax
        mov     ax,[es:bx+4]
        sbb     [freed+4],ax
        mov     ax,[es:bx+6]
        sbb     [freed+6],ax
        mov     ax,[es:bx+8]
        sub     [freed+8],ax
        mov     ax,[es:bx+10]
        sbb     [freed+10],ax
        mov     ax,[es:bx+12]
        sbb     [freed+12],ax
        mov     ax,[es:bx+14]
        sbb     [freed+14],ax
        pop     es
        pop     bx
        ret

; al = 'r': the selected folder, else the one shown, scanned again; 'R': everything
rescan_key:
        mov     bx,[root]
        cmp     al,'R'
        je      .go
        mov     bx,[cur]
        push    bx
        call    selected_entry
        pop     bx
        cmp     ax,-1
        je      .go
        push    bx
        mov     bx,ax
        call    ent_node
        pop     bx
        push    ax
        call    nflags
        test    al,F_DIR
        pop     ax
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
        pushf
        mov     cx,di
        sub     cx,pathbuf
        mov     [pathlen],cx
        popf
        jc      .r
        cmp     cx,PATHMAX              ; a folder too deep to walk: left as it is
        jae     .r
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
        push    es
        mov     es,[del_node]
        xor     bx,bx                   ; freed += its sizes; add_* = minus them
.q:     mov     ax,[es:bx]
        mov     dx,[es:bx+2]
        mov     cx,[es:bx+4]
        mov     si,[es:bx+6]
        add     [freed+bx],ax
        adc     [freed+bx+2],dx
        adc     [freed+bx+4],cx
        adc     [freed+bx+6],si
        not     ax
        not     dx
        not     cx
        not     si
        add     ax,1
        adc     dx,0
        adc     cx,0
        adc     si,0
        mov     [add_app+bx],ax         ; add_disk follows add_app
        mov     [add_app+bx+2],dx
        mov     [add_app+bx+4],cx
        mov     [add_app+bx+6],si
        add     bx,8
        cmp     bx,16
        jb      .q
        mov     ax,[es:N_DESC]          ; -(descendants + 1)
        mov     dx,[es:N_DESC+2]
        add     ax,1
        adc     dx,0
        not     ax
        not     dx
        add     ax,1
        adc     dx,0
        mov     [add_desc],ax
        mov     [add_desc+2],dx
        mov     ax,[es:N_PARENT]
        mov     dx,[es:N_NEXT]
        call    add_up
        mov     bx,[del_node]           ; out of its parent's children
        mov     es,ax
        dec     word [es:N_COUNT]
        mov     ax,[es:N_CHILD]
        cmp     ax,bx
        jne     .walk
        mov     [es:N_CHILD],dx
        pop     es
        ret
.walk:  test    ax,ax
        jz      .r
        mov     es,ax
        mov     ax,[es:N_NEXT]
        cmp     ax,bx
        jne     .walk
        mov     [es:N_NEXT],dx
.r:     pop     es
        ret

include 'SOFTFP.ASM'
include 'PANEL.ASM'
include 'PREVIEW.ASM'
include 'PNG.ASM'
include 'JPEG.ASM'

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
clus            dw 512,0
entseg          dw 0
pendseg         dw 0
old_strategy    dw 0
old_umb_link    db 0
lineseg         dw 0
winseg          dw 0
pend_top        dw 0
arena_next      dw 0
arena_end       dw 0
arena_limit     dw 0
arena_paras     dw 0
root            dw 0
cur             dw 0
nent            dw 0
nfiles          dw 0
zoom            dw 0
sel             dw -1
depth           dw 0
seen            dw 0
last_tick       dw 0
last_input      dw 0
freed           dw 0,0,0,0,0,0,0,0      ; apparent, on disk: as a node's sizes
navsp           dw 0
click_idx       dw -1
left_node       dw 0
presses         dw 0
click_time      dw 0
del_node        dw 0
del_idx         dw 0
del_err         dw 0
del_flags       db 0
del_started     db 0            ; the delete got as far as DOS
st_flags        db 0
dt_flags        db 0
st_node         dw 0
st_di           dw 0
dt_node         dw 0
rs_bytes        dw 0
rm_depth        dw 0
rs_old          dw 0
rs_new          dw 0
rs_parent       dw 0
rs_freed        db 0
rs_target       dw 0
list_cur        dw 0            ; the list's cursor, by node (PANEL.ASM)
cursor_chosen   db 0            ; the entry in hand was picked, not placed by the program

; the board: the treemap's bounds; its borders reach column 79 and row 22
bnd_x           dw PANEL_W
bnd_y           dw 1
bnd_w           dw 79-PANEL_W
bnd_h           dw 21
panel_on        db 1
focus           db 0            ; 0 the list, 1 the treemap

; the treemap's doubles (SOFTFP.ASM), computed at start-up
k_g             dw 0C9FFh,3B9Ah,0,0     ; 999,999,999: DisplaySize's thresholds
k_m             dw 423Fh,000Fh,0,0      ; 999,999
k_k             dw 999,0,0,0

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
s_dot           db ' ',0FAh,' ',0
s_of_scan       db '% of scan',0
s_folders       db ' folders, ',0,' files here',0
s_dirs          db ' dirs, ',0,' files',0
s_disk_used     db 'disk used ',0,', ',0  ; each followed by what goes between the sizes
s_not_scanned   db ' not scanned',0
s_disk          db 'disk ',0,', +',0
s_more          db '... ',0
s_more2         db ' more',0
s_loading       db '...',0
s_skipped       db '  ',0B3h,'  not scanned: too deep',0
s_pct_close     db '%)',0
s_scanning_path db ' ',0
s_hint          db ' Select a tile with the arrow keys, hjkl or the mouse',0
s_zoom          db ' zoom ',0
s_empty         db 'Folder is empty'
s_descendants   db ' descendants)',0
s_trunc_even    db '[...]',0
s_trunc_odd     db '[..]',0
s_legend        db ' ~',1Bh,19h,18h,1Ah,'~ move  ~Tab~ list  ~Enter~ open  ~Esc~ up  ~+-0~ zoom'
                db '  ~a~ size  ~d~ delete  ~q~ quit',0
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
                db '  -k KEYS  type these first; E X T P N H Z are Enter Esc Tab PgUp PgDn Home End',13,10
                db '  -s FILE  then write the screen to FILE and quit',13,10,'$'
msg_notdir      db 'diskonaut: not a folder',13,10,'$'
msg_nomem       db 'diskonaut: not enough memory',13,10,'$'
msg_cpu         db 'diskonaut: needs an 80186, a 286 or later',13,10,'$'

; the treemap's working values
em_x            rb F_SIZE
em_y            rb F_SIZE
em_w            rb F_SIZE
em_h            rb F_SIZE
tot_area        rb F_SIZE
ftotal          rb F_SIZE
ftotal0         rb F_SIZE
eq_pct          rb F_SIZE
f_len           rb F_SIZE
min1            rb F_SIZE
min2            rb F_SIZE
w_sum           rb F_SIZE
w_first         rb F_SIZE
w_second        rb F_SIZE
w_worst         rb F_SIZE
w_cur           rb F_SIZE
l_total         rb F_SIZE
l_prog          rb F_SIZE
l_second        rb F_SIZE
l_tsecond       rb F_SIZE
l_first         rb F_SIZE
r_x             rb F_SIZE
r_y             rb F_SIZE
r_w             rb F_SIZE
r_h             rb F_SIZE
f_t1            rb F_SIZE
f_t2            rb F_SIZE
f_pct           rb F_SIZE
f_area          rb F_SIZE
f_sz            rb F_SIZE
f_unit          rb F_SIZE
fc_one          rb F_SIZE
fc_ratio        rb F_SIZE
fc_min_hr       rb F_SIZE
fc_min_wr       rb F_SIZE
fc_min_prod     rb F_SIZE
fc_100          rb F_SIZE
fc_g            rb F_SIZE
fc_m            rb F_SIZE
fc_k            rb F_SIZE
add_app         dw 0,0,0,0              ; then add_disk: tree_remove relies on it
add_disk        dw 0,0,0,0
add_desc        dw 0,0
ent_size        dw 0,0,0,0
ld_size         dw 0,0,0,0
sum64           dw 0,0,0,0
area_size       dw 0,0,0,0
bf_a            dw 0,0,0,0
area_cached     db 0
pctv            dw 0
pctv_even       dw 0
share10         dw 0
disk_used       dw 0,0,0,0
disk_out        dw 0,0,0,0
disk_known      db 0
lw_first        dw 0
lw_count        dw 0
lw_cursor       dw 0
lw_nlen         dw 0
lw_dirs         dw 0
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
include 'PVDATA.ASM'
include 'FPDATA.ASM'
boxbits         rb 256
scr             rw 80*25
