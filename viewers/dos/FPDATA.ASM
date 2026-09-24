; FPDATA.ASM: SOFTFP.ASM's working storage, included at the end of the data segment. Nothing
; here has a value to begin with, so none of it (or of what comes before it uninitialised) goes
; into the executable.
fz              rw 5
fy              rw 5
fp              rw 8
fr              rw 5
fd              rw 5
fz_int          rw 4
fz_e            rw 1
fr_mode         rw 1
fm_a            rw 1
fz_s            rb 1
fy_s            rb 1
fb_s            rb 1
