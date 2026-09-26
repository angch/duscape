; PANEL.ASM, included by DUSCAPE.ASM: the side panel left of the treemap, as
; viewers/tui/src/ui/side_panel.rs draws it on an 80-column screen: a third of the width, the
; folder's details, its entries largest first, and under them the preview of the entry in hand.
;
; The keyboard is on the list (focus 0) or the treemap (1), as in the terminal viewer: the
; list's cursor is kept by node, since the listing re-sorts while a scan runs, and while the list
; has the keyboard the treemap selects the cursor's tile, or none if it has none.

PANEL_W         = 26            ; 80 / 3
PANEL_TEXT      = PANEL_W-1     ; the last column is left blank
LIST_Y          = 1
HEADER_ROWS     = 4
LIST_ROWS       = 10            ; 14 rows of list, less the header
PREVIEW_Y       = 15            ; the caption; the body under it
PV_COLS         = PANEL_TEXT
PV_ROWS         = 7
PV_HROWS        = 2*PV_ROWS
ACC_SIZE        = 32
IOBUF           = 8192

A_HEADING       = 0Ah
A_DETAILS       = 0Eh
A_CAPTION       = 0Bh
A_CURSOR        = 70h           ; the cursor, with the keyboard
A_CURSOR_AWAY   = 1Fh           ; without
A_ONBOARD       = 0Fh
A_OFFBOARD      = 07h

; the board's bounds for the panel shown or not
panel_layout:
        mov     [bnd_x],0
        mov     [bnd_w],79
        cmp     [panel_on],0
        je      @f
        mov     [bnd_x],PANEL_W
        mov     [bnd_w],79-PANEL_W
@@:     ret

; ax ← where the list's cursor is in the listing: its entry, or the top until one is chosen; -1
; for an empty folder
list_pos:
        call    list_cursor_index
        cmp     ax,-1
        jne     .r
        xor     ax,ax
        cmp     [nent],0
        jne     .r
        dec     ax
.r:     ret

; ax ← the listing index of the list cursor's entry, or -1 if there is none or it has gone
list_cursor_index:
        mov     dx,[list_cur]
        mov     ax,-1
        test    dx,dx
        jz      .r
        xor     bx,bx
        push    es
        mov     es,[entseg]
        xor     si,si
.l:     cmp     bx,[nent]
        jae     .none
        cmp     [es:si],dx
        je      .f
        inc     bx
        add     si,2
        jmp     .l
.none:  pop     es
        ret
.f:     pop     es
        mov     ax,bx
.r:     ret

; ax = a listing index → ax = its tile, or -1
tile_of:
        push    cx
        push    si
        xor     cx,cx
        mov     si,tiles
.l:     cmp     cx,[ntiles]
        jae     .none
        cmp     [si+T_IDX],ax
        je      .f
        inc     cx
        add     si,T_SIZE
        jmp     .l
.none:  mov     cx,-1
.f:     mov     ax,cx
        pop     si
        pop     cx
        ret

; ax ← the entry in hand, as a listing index or -1: the list's with the list focused, else the
; selected tile's
selected_entry:
        cmp     [panel_on],0
        je      .board
        cmp     [focus],0
        jne     .board
        jmp     list_pos
.board: mov     ax,[sel]
        cmp     ax,-1
        je      .r
        shl     ax,4
        mov     si,ax
        mov     ax,word [tiles+si+T_IDX]
.r:     ret

; with the list focused, the treemap shows the list's entry (sync_board_to_list)
sync_board:
        cmp     [panel_on],0
        je      .r
        cmp     [focus],0
        jne     .r
        call    list_pos
        cmp     ax,-1
        je      .none
        call    tile_of
.none:  mov     [sel],ax
.r:     ret

; the list's cursor to listing index ax, and its tile selected (set_list_cursor)
set_list_cursor:
        cmp     ax,[nent]
        jae     .r
        push    bx
        mov     bx,ax
        push    ax
        call    ent_node
        mov     [list_cur],ax
        pop     ax
        pop     bx
        call    tile_of
        mov     [sel],ax
.r:     ret

; ax = rows to move, signed, stopping at either end (move_list_cursor)
move_list:
        cmp     [nent],0
        je      .r
        push    ax
        call    list_pos
        pop     dx
        add     ax,dx                   ; both within ±32767: an overflow says which end
        jno     @f
        mov     ax,7FFFh
        test    dx,dx
        jns     @f
        xor     ax,ax
@@:     test    ax,ax
        jns     @f
        xor     ax,ax
@@:     mov     dx,[nent]
        dec     dx
        cmp     ax,dx
        jle     @f
        mov     ax,dx
@@:     call    set_list_cursor
.r:     ret

focus_list:
        cmp     [panel_on],0
        je      .r
        mov     [focus],0
        call    list_cursor_index
        cmp     ax,-1
        jne     .r
        call    selected_entry.board    ; start where the treemap's selection is, or at the top
        cmp     ax,-1
        jne     @f
        xor     ax,ax
@@:     call    set_list_cursor
.r:     ret

focus_treemap:
        mov     [focus],1
        call    list_cursor_index       ; carry the list's entry over, if it has a tile
        cmp     ax,-1
        je      .r
        call    tile_of
        cmp     ax,-1
        je      .r
        mov     [sel],ax
.r:     ret

; the first shown row and how many: the cursor kept in the middle, the last row left for
; "N more" (list_window)
list_window:
        mov     cx,[nent]
        xor     bx,bx
        cmp     cx,LIST_ROWS
        jbe     .r
        mov     cx,LIST_ROWS-1
        push    cx
        call    selected_entry          ; around the entry highlighted, whichever panel
        pop     cx
        cmp     ax,-1
        jne     @f
        xor     ax,ax
@@:     sub     ax,(LIST_ROWS-1)/2
        jns     @f
        xor     ax,ax
@@:     mov     dx,[nent]
        sub     dx,cx
        cmp     ax,dx
        jbe     @f
        mov     ax,dx
@@:     mov     bx,ax
.r:     ret

; cx = column, dx = row → ax = the listing index shown there, or -1 (entry_at)
entry_at:
        mov     ax,-1
        cmp     [panel_on],0
        je      .r
        cmp     cx,PANEL_W
        jae     .r
        cmp     dx,LIST_Y+HEADER_ROWS
        jb      .r
        cmp     dx,LIST_Y+HEADER_ROWS+LIST_ROWS
        jae     .r
        push    dx
        call    list_window
        pop     dx
        sub     dx,LIST_Y+HEADER_ROWS
        cmp     dx,cx
        mov     ax,-1
        jae     .r
        mov     ax,bx
        add     ax,dx
.r:     ret

; ds:si, cx = length → tb as `fit` makes it: cut in the middle with '~', padded to PANEL_TEXT
fit_panel:
        push    es
        push    ds
        pop     es
        mov     di,tb
        cmp     cx,PANEL_TEXT
        jbe     .copy
        mov     bx,(PANEL_TEXT-1)/2     ; the tail
        push    si
        push    cx
        mov     cx,PANEL_TEXT-1-(PANEL_TEXT-1)/2
        rep     movsb
        mov     al,'~'
        stosb
        pop     cx
        pop     si
        add     si,cx
        sub     si,bx
        mov     cx,bx
.copy:  rep     movsb
        mov     cx,tb+PANEL_TEXT
        sub     cx,di
        mov     al,' '
        rep     stosb
        pop     es
        ret

; tb, PANEL_TEXT wide, on row dh in ah
panel_line:
        push    ax
        mov     dl,0
        mov     si,tb
        mov     cx,PANEL_TEXT
        pop     ax
        call    put_str
        ret

; the entry at listing index bx → l1buf, cx = length: its name, '/' for a folder
entry_name:
        push    ax
        call    ent_node
        mov     di,l1buf
        push    ax
        call    nname
        pop     ax
        call    nflags
        test    al,F_DIR
        jz      @f
        mov     byte [di],'/'
        inc     di
@@:     mov     cx,di
        sub     cx,l1buf
        pop     ax
        ret

draw_panel:
        cmp     [panel_on],0
        je      .r
        ; the folder's path
        mov     di,l1buf
        mov     ax,[cur]
        call    node_path
        mov     cx,di
        sub     cx,l1buf
        mov     si,l1buf
        call    fit_panel
        mov     dh,LIST_Y
        mov     ah,A_HEADING
        call    panel_line
        call    draw_panel_sizes
        call    draw_panel_counts
        call    draw_panel_disk
        ; the entries
        call    list_window             ; bx = first, cx = how many
        mov     [lw_first],bx
        mov     [lw_count],cx
        call    selected_entry
        mov     [lw_cursor],ax
        xor     bp,bp
.row:   cmp     bp,[lw_count]
        jae     .more
        mov     bx,[lw_first]
        add     bx,bp
        push    bx
        call    entry_name              ; the name, cut to fit beside a 7-wide size
        mov     [lw_nlen],cx
        pop     bx
        push    bx
        ; the row: name, then the size right-aligned in 7
        push    ds
        pop     es
        mov     di,l2buf
        mov     al,' '
        mov     cx,PANEL_TEXT
        rep     stosb
        mov     cx,[lw_nlen]
        cmp     cx,PANEL_TEXT-7
        jbe     @f
        call    name_cut
@@:     mov     si,l1buf
        mov     di,l2buf
        rep     movsb
        pop     bx
        push    bx
        call    ent_node
        call    nsize
        mov     di,s1buf
        mov     si,f_sz
        call    fmt_size
        mov     cx,di
        sub     cx,s1buf
        mov     di,l2buf+PANEL_TEXT
        sub     di,cx
        mov     si,s1buf
        rep     movsb
        ; its colours
        pop     bx
        mov     ah,A_CURSOR
        cmp     bx,[lw_cursor]
        jne     .notcur
        cmp     [focus],0
        je      .put
        mov     ah,A_CURSOR_AWAY
        jmp     .put
.notcur:
        mov     ax,bx
        call    tile_of
        cmp     ax,-1
        mov     ah,A_OFFBOARD
        je      .put
        mov     ah,A_ONBOARD
        push    ax
        call    ent_node
        call    nflags
        test    al,F_DIR
        pop     ax
        jz      .put
        mov     ah,A_DIR
.put:   mov     dx,bp
        add     dx,LIST_Y+HEADER_ROWS
        mov     dh,dl
        mov     dl,0
        mov     si,l2buf
        mov     cx,PANEL_TEXT
        call    put_str
        inc     bp
        jmp     .row
.more:  mov     ax,[nent]
        sub     ax,[lw_count]
        jz      .preview
        push    ds
        pop     es
        mov     di,l2buf
        mov     si,s_more
        call    append_z
        mov     ax,[nent]
        sub     ax,[lw_count]
        xor     dx,dx
        call    u32dec_c
        mov     si,s_more2
        call    append_z
        mov     cx,di
        sub     cx,l2buf
        mov     si,l2buf
        call    fit_panel
        mov     dx,[lw_count]
        add     dx,LIST_Y+HEADER_ROWS
        mov     dh,dl
        mov     ah,A_OFFBOARD
        call    panel_line
.preview:
        call    draw_preview
.r:     ret

; the name in l1buf, cx long, cut to PANEL_TEXT-7 in the middle with '~' → cx
name_cut:
        push    si
        push    di
        mov     si,l1buf
        push    cx
        ; fit_panel cuts to PANEL_TEXT; this is narrower, so do it here
        mov     bx,(PANEL_TEXT-8)/2     ; tail
        mov     dx,PANEL_TEXT-8-(PANEL_TEXT-8)/2
        mov     di,trbuf
        mov     cx,dx
        rep     movsb
        mov     al,'~'
        stosb
        pop     cx
        mov     si,l1buf
        add     si,cx
        sub     si,bx
        mov     cx,bx
        rep     movsb
        mov     cx,PANEL_TEXT-7
        mov     si,trbuf
        mov     di,l1buf
        push    cx
        rep     movsb
        pop     cx
        pop     di
        pop     si
        ret

; "5.2G · 68,175 files · 12.5% of scan", or less of it; the share only below the root, as the
; terminal viewer has no scan total to give at the top
draw_panel_sizes:
        push    ds
        pop     es
        mov     [share10],-1
        mov     ax,[cur]
        cmp     ax,[root]
        je      .size
        call    nsize                   ; size / total × 100, then {:.1}
        mov     si,f_sz
        mov     bx,f_t1
        call    f_copy
        mov     ax,[root]
        call    nsize
        cmp     byte [f_sz+F_C],FC_ZERO
        je      .size
        mov     si,f_t1
        mov     di,f_sz
        mov     bx,f_t1
        call    f_div
        mov     di,fc_100
        call    f_mul
        mov     cx,10
        call    f_round_even
        mov     [share10],ax
.size:  mov     ax,[cur]                ; the size
        call    nsize
        mov     di,s1buf
        mov     si,f_sz
        call    fmt_size
        mov     [s1len],di
        ; the first that fits
        mov     di,l2buf
        call    .sz
        mov     si,s_dot
        call    append_z
        mov     ax,[cur]
        call    ndesc
        call    u32dec_c
        mov     si,s_files
        call    append_z
        call    .share
        call    .fits
        jbe     .put
        mov     di,l2buf
        call    .sz
        call    .share
        call    .fits
        jbe     .put
        mov     di,l2buf
        call    .sz
.put:   mov     cx,di
        sub     cx,l2buf
        mov     si,l2buf
        call    fit_panel
        mov     dh,LIST_Y+1
        mov     ah,A_DETAILS
        jmp     panel_line
.sz:    mov     si,s1buf
        mov     cx,[s1len]
        sub     cx,s1buf
        rep     movsb
        ret
.share: cmp     [share10],-1
        je      @f
        mov     si,s_dot
        call    append_z                ; lodsb: AL is not safe across it
        mov     ax,[share10]
        xor     dx,dx
        call    div10
        call    u32dec
        mov     al,'.'
        stosb
        mov     al,bl
        add     al,'0'
        stosb
        mov     si,s_of_scan
        call    append_z
@@:     ret
.fits:  mov     ax,di
        sub     ax,l2buf
        cmp     ax,PANEL_TEXT
        ret

; "3 folders, 12 files here", or "3 dirs, 12 files"
draw_panel_counts:
        push    ds
        pop     es
        xor     bx,bx
        xor     dx,dx                   ; folders
.l:     cmp     bx,[nent]
        jae     .c
        call    ent_node
        call    nflags
        test    al,F_DIR
        jz      @f
        inc     dx
@@:     inc     bx
        jmp     .l
.c:     mov     [lw_dirs],dx
        mov     si,s_folders
        call    .text
        cmp     cx,PANEL_TEXT
        jbe     .put
        mov     si,s_dirs
        call    .text
.put:   mov     si,l2buf
        call    fit_panel
        mov     dh,LIST_Y+2
        mov     ah,A_DETAILS
        jmp     panel_line
.text:  mov     di,l2buf
        mov     ax,[lw_dirs]
        xor     dx,dx
        call    u32dec_c
        call    append_z                ; " folders, " or " dirs, "
        mov     ax,[nent]
        sub     ax,[lw_dirs]
        xor     dx,dx
        call    u32dec_c
        call    append_z                ; " files here" or " files": the next string
        mov     cx,di
        sub     cx,l2buf
        ret

; "disk used 9.1G, 3.9G not scanned": at the top, once the scan is done, sizes on disk, when
; the drive says more is used than the scan found and has freed since (as the terminal viewer)
draw_panel_disk:
        cmp     [disk_known],0
        je      .r
        cmp     [scanning],0
        jne     .r
        cmp     [apparent],0
        jne     .r
        mov     ax,[cur]
        cmp     ax,[root]
        jne     .r
        push    es
        mov     es,ax
        mov     ax,[es:N_DISK]          ; outside = used - (found + freed)
        mov     dx,[es:N_DISK+2]
        mov     cx,[es:N_DISK+4]
        mov     bx,[es:N_DISK+6]
        pop     es
        add     ax,[freed+N_DISK]
        adc     dx,[freed+N_DISK+2]
        adc     cx,[freed+N_DISK+4]
        adc     bx,[freed+N_DISK+6]
        mov     si,[disk_used]
        sub     si,ax
        mov     [disk_out],si
        mov     si,[disk_used+2]
        sbb     si,dx
        mov     [disk_out+2],si
        mov     si,[disk_used+4]
        sbb     si,cx
        mov     [disk_out+4],si
        mov     si,[disk_used+6]
        sbb     si,bx
        mov     [disk_out+6],si
        jnb     @f
        mov     [disk_out],0            ; the drive says less: 0 not scanned (saturating_sub)
        mov     [disk_out+2],0
        mov     [disk_out+4],0
        mov     [disk_out+6],0
@@:
        push    ds
        pop     es
        mov     di,l2buf
        mov     si,s_disk_used
        call    .both
        mov     si,s_not_scanned
        call    append_z
        mov     cx,di
        sub     cx,l2buf
        cmp     cx,PANEL_TEXT
        jbe     .put
        mov     di,l2buf
        mov     si,s_disk
        call    .both
        mov     cx,di
        sub     cx,l2buf
.put:   mov     si,l2buf
        call    fit_panel
        mov     dh,LIST_Y+3
        mov     ah,A_DETAILS
        jmp     panel_line
.r:     ret
; si's text, the used size, ", " or ", +" (the string after si's), the outside size
.both:  call    append_z
        push    si
        mov     si,disk_used
        mov     bx,f_sz
        call    f_from_u64
        mov     si,f_sz
        call    fmt_size
        pop     si
        call    append_z
        mov     si,disk_out
        mov     bx,f_sz
        call    f_from_u64
        mov     si,f_sz
        jmp     fmt_size

; the caption and the preview under the list
draw_preview:
        call    selected_entry
        cmp     ax,-1
        je      .r
        mov     bx,ax
        push    bx
        call    entry_name              ; "name · 5.2K · PNG 640x480"
        push    ds
        pop     es
        mov     di,l1buf
        add     di,cx
        mov     si,s_dot
        call    append_z
        pop     bx
        call    ent_node
        call    nsize
        mov     si,f_sz
        call    fmt_size
        cmp     [pv_state],PV_PICTURE
        jne     @f
        mov     si,s_dot
        call    append_z
        mov     si,pv_desc
        call    append_z
@@:     mov     cx,di
        sub     cx,l1buf
        mov     si,l1buf
        call    fit_panel
        mov     dh,PREVIEW_Y
        mov     ah,A_CAPTION
        call    panel_line
        mov     al,[pv_state]
        cmp     al,PV_TEXT
        je      .text
        cmp     al,PV_INFO
        je      .info
        cmp     al,PV_PENDING
        je      .loading
        cmp     al,PV_PICTURE
        jne     .r
        mov     dl,0
        mov     dh,PREVIEW_Y+1
        jmp     draw_blocks
.loading:
        mov     si,s_loading
        jmp     .one
.info:  mov     si,pv_info
.one:   push    si
        xor     cx,cx
@@:     lodsb
        test    al,al
        jz      @f
        inc     cx
        jmp     @b
@@:     pop     si
        cmp     cx,PANEL_TEXT
        jbe     @f
        mov     cx,PANEL_TEXT
@@:     mov     dl,0
        mov     dh,PREVIEW_Y+1
        mov     ah,A_TEXT
        jmp     put_str
.text:  xor     bx,bx
.tl:    cmp     bl,[pv_nlines]
        jae     .r
        mov     si,bx
        shl     si,5
        add     si,pv_text
        mov     cl,[pv_tlen+bx]
        xor     ch,ch
        mov     dl,0
        mov     dh,PREVIEW_Y+1
        add     dh,bl
        mov     ah,A_TEXT
        push    bx
        call    put_str
        pop     bx
        inc     bx
        jmp     .tl
.r:     ret
