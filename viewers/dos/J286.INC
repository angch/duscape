; J286.INC: conditional jumps a 286 can take. The 386 has near conditional jumps (0F 8x);
; the 286 has only short ones. Each is short when its target is in reach, else the opposite
; condition jumps over a near jmp. Emitted as bytes, since a macro cannot use its own mnemonic.

macro jcc286 cc, target
{
  local dist
  dist = (target) - ($ + 2)
  if dist >= -128 & dist <= 127
    db 70h + cc, dist
  else
    db 70h + (cc xor 1), 3, 0E9h
    dw (target) - ($ + 2)
  end if
}

macro jo t { jcc286 0, t }
macro jno t { jcc286 1, t }
macro jb t { jcc286 2, t }
macro jc t { jcc286 2, t }
macro jnae t { jcc286 2, t }
macro jae t { jcc286 3, t }
macro jnb t { jcc286 3, t }
macro jnc t { jcc286 3, t }
macro je t { jcc286 4, t }
macro jz t { jcc286 4, t }
macro jne t { jcc286 5, t }
macro jnz t { jcc286 5, t }
macro jbe t { jcc286 6, t }
macro jna t { jcc286 6, t }
macro ja t { jcc286 7, t }
macro jnbe t { jcc286 7, t }
macro js t { jcc286 8, t }
macro jns t { jcc286 9, t }
macro jp t { jcc286 10, t }
macro jpe t { jcc286 10, t }
macro jnp t { jcc286 11, t }
macro jpo t { jcc286 11, t }
macro jl t { jcc286 12, t }
macro jnge t { jcc286 12, t }
macro jge t { jcc286 13, t }
macro jnl t { jcc286 13, t }
macro jle t { jcc286 14, t }
macro jng t { jcc286 14, t }
macro jg t { jcc286 15, t }
macro jnle t { jcc286 15, t }
