; PVDATA.ASM, included in DUSCAPE.ASM's data segment: what PREVIEW.ASM, PNG.ASM and JPEG.ASM
; keep. Words only: a dword variable would get 32-bit instructions from FASM.

pv_node         dw 0                    ; the file previewed, or 0
pv_state        db PV_NONE
pv_kind         db 0
pv_nlines       db 0
pal_applied     db 0                    ; the DAC has the picture's colours
pal_want        db 0
acc_mode        db 0                    ; 0 RGBA sums, 1 Y Cb Cr
hd_wrapped      db 0
u8_len          db 0
u8_bad          db 0                    ; an invalid byte seen in the head
u8_lo           db 80h,0BFh             ; the next byte's range
near_c          db 0
pv_len          dw 0,0                  ; the head's length, up to 65536
sn_hi           dw 0
pv_handle       dw 0
io_pos          dw 0
io_len          dw 0
dec_sp          dw 0
dec_err         dw 0
img_w           dw 0
img_h           dw 0
fit_w           dw 0
fit_h           dw 0
acc_cols        dw 1
acc_hrows       dw 1
step_x          dw 1
step_y          dw 1
acc_base        dw 0
acc_n           dw 0
jx              dw 0
jy              dw 0
ac_v            dw 0
ac_w            dw 0
px_r            db 0
px_g            db 0
px_b            db 0
px_a            db 0
ycc             dw 0,0,0,0,0,0          ; Y, Cb, Cr, at 0, 4, 8
far_d           dw 0,0
far_i           dw 0
near_d          dw 0,0
dist_sum          dw 0,0
dist_weight       db 2,4,3                ; the weights of red, green and blue in a distance
adapt_used      dw 0
; the colours pictures may have: none of them is the interface's
adaptive        db 2,5,6,8,12,13
ADAPT_N         = $ - adaptive

s_pathlong      db 'path too long for DOS',0
s_binary        db 'binary file',0
s_emptyfile     db 'empty file',0
s_unreadable    db 'could not read it',0
s_doserr_paren  db ' (DOS error ',0
s_truncated     db 'truncated',0
s_too_large     db 'too large',0
s_png_unreadable db 'PNG image, unreadable: ',0
s_jpeg_unreadable db 'JPEG image, unreadable: ',0
s_png           db 'PNG ',0
s_png_noidat    db 'no image data',0
s_png_bad       db 'corrupt',0
s_too_wide      db 'too wide for DOS',0
s_jpeg          db 'JPEG ',0
s_jpeg_kind     db 'not a kind DOS duscape decodes',0
s_jpeg_cmyk     db 'CMYK',0
s_jpeg_noscan   db 'no image data',0
s_jpeg_bad      db 'corrupt',0

; CP437's upper half, as Unicode: the text preview's way back from UTF-8
cp437_hi:
                dw 000C7h,000FCh,000E9h,000E2h,000E4h,000E0h,000E5h,000E7h
                dw 000EAh,000EBh,000E8h,000EFh,000EEh,000ECh,000C4h,000C5h
                dw 000C9h,000E6h,000C6h,000F4h,000F6h,000F2h,000FBh,000F9h
                dw 000FFh,000D6h,000DCh,000A2h,000A3h,000A5h,020A7h,00192h
                dw 000E1h,000EDh,000F3h,000FAh,000F1h,000D1h,000AAh,000BAh
                dw 000BFh,02310h,000ACh,000BDh,000BCh,000A1h,000ABh,000BBh
                dw 02591h,02592h,02593h,02502h,02524h,02561h,02562h,02556h
                dw 02555h,02563h,02551h,02557h,0255Dh,0255Ch,0255Bh,02510h
                dw 02514h,02534h,0252Ch,0251Ch,02500h,0253Ch,0255Eh,0255Fh
                dw 0255Ah,02554h,02569h,02566h,02560h,02550h,0256Ch,02567h
                dw 02568h,02564h,02565h,02559h,02558h,02552h,02553h,0256Bh
                dw 0256Ah,02518h,0250Ch,02588h,02584h,0258Ch,02590h,02580h
                dw 003B1h,000DFh,00393h,003C0h,003A3h,003C3h,000B5h,003C4h
                dw 003A6h,00398h,003A9h,003B4h,0221Eh,003C6h,003B5h,02229h
                dw 02261h,000B1h,02265h,02264h,02320h,02321h,000F7h,02248h
                dw 000B0h,02219h,000B7h,0221Ah,0207Fh,000B2h,025A0h,000A0h

; PNG
png_hdr         db 0
png_plte        db 0
png_trns        db 0                    ; tRNS gave gray or RGB a transparent colour
png_key         db 0,0,0,0,0,0          ; that colour's samples, 16 bits each, big-endian
png_depth       db 0
png_ctype       db 0
png_interlace   db 0
png_chans       db 0
png_done        db 0
png_pass        db 0
zlast           db 0
bitcnt          db 0
bitbuf          dw 0
png_bpx         dw 0
png_bpp         dw 0
line_bytes      dw 0
line_len        dw 0
lpos            dw 0
cur_base        dw 0
prev_base       dw 0
wpos            dw 0
pa              dw 0
pb              dw 0
pc              dw 0
pv_b            dw 0
zb_tab          dw 0
znlen           dw 0
zndist          dw 0
zncode          dw 0
zntotal         dw 0
chunk_len       dw 0,0
idat_left       dw 0,0
pass_x0         dw 0
pass_y0         dw 0
pass_dx         dw 1
pass_dy         dw 1
pass_w          dw 0
pass_h          dw 0
pass_row        dw 0
pstep_x         dw 1
pstep_y         dw 1
zorder          db 16,17,18,0,8,7,9,6,10,5,11,4,12,3,13,2,14,1,15
zlext           db 0,0,0,0,0,0,0,0,1,1,1,1,2,2,2,2,3,3,3,3,4,4,4,4,5,5,5,5,0
zdext           db 0,0,0,0,1,1,2,2,3,3,4,4,5,5,6,6,7,7,8,8,9,9,10,10,11,11,12,12,13,13
                db 0
zlbase          dw 3,4,5,6,7,8,9,10,11,13,15,17,19,23,27,31,35,43,51,59,67,83,99,115
                dw 131,163,195,227,258
zdbase          dw 1,2,3,4,5,7,9,13,17,25,33,49,65,97,129,193,257,385,513,769,1025,1537
                dw 2049,3073,4097,6145,8193,12289,16385,24577

; JPEG
jsof            db 0
jdone           db 0                    ; the components that have their means, a bit each
jmarker         db 0
jprog           db 0
jnf             db 0
jhmax           db 1
jvmax           db 1
jss             db 0
jse             db 0
jah             db 0
jal             db 0
jcnt            db 0
jbits_buf       db 0
jcur            db 0
jv              db 0
jh              db 0
jslot           db 0
jhdef           db 0                    ; the Huffman tables defined: DC 0-3, AC 4-7
jqdef           db 0                    ; the quantisation tables defined
jc_id           db 0,0,0,0
jc_h            db 0,0,0,0
jc_v            db 0,0,0,0
jc_tq           db 0,0,0,0
jc_td           db 0,0,0,0
jc_ta           db 0,0,0,0
jsc             db 0,0,0,0
jstep_x         db 1,1,1,1
jstep_y         db 1,1,1,1
jnscan          dw 0
jri             dw 0
jtogo           dw 0
jq              dw 1,1,1,1
jpred           dw 0,0,0
jbw             dw 0
jbh             dw 0
jbx             dw 0
jby             dw 0
jmcux           dw 0
jmcuy           dw 0
jmx             dw 0
jmy             dw 0
jsc_num         dw 0
jsc_den         dw 1
jtmp            rw 16

; and the big ones, not initialised
dacidx          rb 16
pal_rgb         rb 48
pal_orig        rb 48
pal_pic         rb 48
rsum            rw ADAPT_N*8
pv_tlen         rb PV_ROWS
pv_text         rb PV_ROWS*32
pv_info         rb 80
pv_desc         rb 40
pvpath          rb 300
blk             rb PV_COLS*PV_HROWS
tcol            rb PV_COLS*PV_HROWS*4
acc             rw PV_COLS*PV_HROWS*ACC_SIZE/2
pal             rb 768
pal_a           rb 256
lencode         rw 16+288
distcode        rw 16+32
zoffs           rw 17
zlens           rb 320
jhuff           rb 8*J_TABLE
iobuf           rb IOBUF
