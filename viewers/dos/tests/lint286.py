#!/usr/bin/env python3
"""Every instruction in the DOS viewer's sources that a 286 does not have.

    python3 viewers/dos/tests/lint286.py        # exits 1 and lists them if there are any

32-bit registers and operands, FS and GS, 32-bit addressing, the 386's own instructions and
every x87 one; and variables declared dd or dq, for which FASM makes 32-bit instructions without
being asked. Conditional jumps are J286.INC's macros, so they cannot be near ones.
"""
import os, re, sys

HERE = os.path.dirname(os.path.abspath(__file__))
SOURCES = ["DISKONAU.ASM", "PANEL.ASM", "PREVIEW.ASM", "PNG.ASM", "JPEG.ASM", "SOFTFP.ASM",
           "PVDATA.ASM", "FPDATA.ASM"]
BAD = [
    (r"\be(ax|bx|cx|dx|si|di|bp|sp)\b", "a 32-bit register"),
    (r"\b(fs|gs)\b", "FS or GS"),
    (r"\bdword\b", "a 32-bit operand"),
    (r"\b(movzx|movsx|jecxz|pushad|popad|pushfd|popfd|cdq|cwde|bswap|shld|shrd|bsf|bsr|"
     r"lodsd|stosd|movsd|cmpsd|scasd|insd|outsd|bt|bts|btr|btc|set[a-z]{1,3}|lfs|lgs|lss|"
     r"cmpxchg|xadd|cpuid|rdtsc)\b", "a 386 or later instruction"),
    (r"\bf[a-z]{2,7}\b", None),   # x87: checked against the list below
    (r"\bqword\b|\btword\b", "an x87 operand"),
]
X87 = set("""fld fst fstp fild fist fistp fadd faddp fsub fsubp fsubr fsubrp fmul fmulp fdiv fdivp
fdivr fdivrp fcom fcomp fcompp fucom fucomp ftst fxam fnstsw fstsw fldcw fnstcw fstcw fninit finit
fchs fabs frndint fsqrt fxch fldz fld1 fiadd fisub fisubr fimul fidiv fidivr ficom ficomp fbld
fbstp fscale fprem fwait fclex fnclex""".split())


def main():
    bad = 0
    for name in SOURCES:
        path = os.path.join(HERE, "..", name)
        for number, line in enumerate(open(path, encoding="utf-8"), 1):
            code = line.split(";", 1)[0]
            code = re.sub(r"'[^']*'", "''", code)          # strings are not instructions
            if re.match(r"\s*([a-z_0-9.]+:?\s+)?(dd|dq|rd|rq)\s", code, re.I):
                # FASM gives a dword or qword variable 32-bit or x87 instructions unasked
                print(f"{name}:{number}: a 32 or 64-bit variable: {line.rstrip()}")
                bad += 1
                continue
            if re.match(r"\s*[a-z_0-9.]*\s*(d[bwdq]|r[bwdq]|=|equ)\b", code) or not code.strip():
                continue
            for pattern, why in BAD:
                for match in re.finditer(pattern, code, re.I):
                    if why is None:
                        if match.group(0).lower() not in X87:
                            continue
                        why = "an x87 instruction"
                    print(f"{name}:{number}: {why}: {line.rstrip()}")
                    bad += 1
                    break
    print(f"{bad} lines a 286 cannot run" if bad else "nothing a 286 cannot run")
    sys.exit(1 if bad else 0)


if __name__ == "__main__":
    main()
