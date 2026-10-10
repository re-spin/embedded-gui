/* _spike_index_key_row(u16 *dst, const u8 *src, unsigned n, const u16 *lut):
   one indexed8 row with transparent index 255, C ABI (lut on the stack).
   dst[i] = lut[src[i]] unless src[i] == 255.

   Pixels one at a time until src is 4-aligned, then a word of four indices
   at a time.  A word with no 255 (haszero(~w): (~w - 0x01010101) & w &
   0x80808080 == 0) is four plain lookups with no taken branch: indices by
   EXTRACT, all table addresses before the first load (no DAG stall), and
   each load issued with the previous pixel's store through I1.  An all-255
   word (a transparent run) is tested first and skips its four pixels; a
   word with some 255 is two pairs where a kept pixel keeps its old value
   through a conditional move.  Both branch out of the loop body and back
   to its last instruction.  Then the 0-3
   pixels left.  P3 (and I1 in the word loop) is the next dst pixel.
   Writes R0-R3, P0-P2, I1, M1, LC0, LT0, LB0, CC; saves R4-R7, P3-P5.  */
	.section .text.spike_index_key_row,"ax",@progbits
	.align 4
	.global _spike_index_key_row
	.type _spike_index_key_row, STT_FUNC
_spike_index_key_row:
	P2 = [SP + 12];
	[--SP] = (R7:4, P5:3);
	P3 = R0;
	P1 = R1;
	R5 = 255;
	R6 = R2;			/* pixels left */
.Lhead:
	CC = R6 == 0;
	IF CC JUMP .Ldone;
	R0 = P1;
	R3 = 3;
	R0 = R0 & R3;
	CC = R0 == 0;
	IF CC JUMP .Lwords;
	R1 = B[P1++] (Z);
	R6 += -1;
	P0 = R1;
	CC = R1 == R5;
	R4 = W[P3] (Z);
	P0 = P2 + (P0 << 1);
	R2 = W[P0] (Z);
	IF CC R2 = R4;
	W[P3++] = R2;
	JUMP .Lhead;
.Lwords:
	R3 = R6 >> 2;
	R0 = 3;
	R6 = R6 & R0;
	M1 = R6;			/* the tail */
	CC = R3 == 0;
	IF CC JUMP .Ltail0;
	R6.H = 0x0101;
	R6.L = 0x0101;
	R7.H = 0x8080;
	R7.L = 0x8080;
	R2 = 0x0808 (Z);		/* EXTRACT byte 1 */
	R5 = 0x1008 (Z);		/* EXTRACT byte 2 */
	I1 = P3;
	P5 = R3;
	LSETUP (.Lw, .Lw_end) LC0 = P5;
.Lw:
	R0 = [P1++];
	R1 = ~R0;
	CC = R1 == 0;
	IF CC JUMP .Lskip;
	R1 = R1 - R6;
	R1 = R1 & R0;
	R1 = R1 & R7;
	CC = R1 == 0;
	IF !CC JUMP .Lkeyed;
	R1 = R0.B (Z);
	R3 = EXTRACT (R0, R2.L) (Z);
	P0 = R1;
	R4 = EXTRACT (R0, R5.L) (Z);
	P4 = R3;
	R0 >>= 24;
	P5 = R4;
	P0 = P2 + (P0 << 1);
	P4 = P2 + (P4 << 1);
	P5 = P2 + (P5 << 1);
	R1 = W[P0] (Z);
	P0 = R0;
	MNOP || R3 = W[P4] (Z) || W[I1++] = R1.L;
	P0 = P2 + (P0 << 1);
	MNOP || R4 = W[P5] (Z) || W[I1++] = R3.L;
	MNOP || R1 = W[P0] (Z) || W[I1++] = R4.L;
	W[I1++] = R1.L;
.Lw_end:
	P3 += 8;
	JUMP .Ltail0;
.Lskip:
	I1 += 4;
	I1 += 4;
	JUMP .Lw_end;
.Ltail0:
	R6 = M1;
	R5 = 255;
.Ltail:
	CC = R6 == 0;
	IF CC JUMP .Ldone;
	R1 = B[P1++] (Z);
	R6 += -1;
	P0 = R1;
	CC = R1 == R5;
	R4 = W[P3] (Z);
	P0 = P2 + (P0 << 1);
	R2 = W[P0] (Z);
	IF CC R2 = R4;
	W[P3++] = R2;
	JUMP .Ltail;
.Ldone:
	(R7:4, P5:3) = [SP++];
	RTS;
/* A word with a 255, out of the word loop so a plain word takes no branch;
   it returns to the loop's last instruction with I1 moved past the word.  */
.Lkeyed:
	I1 += 4;
	I1 += 4;
	R5 = 255;
	R1 = R0.B (Z);
	R3 = R0 >> 8;
	R3 = R3.B (Z);
	P0 = R1;
	P4 = R3;
	CC = R1 == R5;
	R4 = W[P3] (Z);
	P0 = P2 + (P0 << 1);
	P4 = P2 + (P4 << 1);
	R2 = W[P0] (Z);
	IF CC R2 = R4;
	W[P3] = R2;
	CC = R3 == R5;
	R4 = W[P3 + 2] (Z);
	R2 = W[P4] (Z);
	IF CC R2 = R4;
	W[P3 + 2] = R2;
	R1 = R0 >> 16;
	R1 = R1.B (Z);
	R3 = R0 >> 24;
	P0 = R1;
	P4 = R3;
	CC = R1 == R5;
	R4 = W[P3 + 4] (Z);
	P0 = P2 + (P0 << 1);
	P4 = P2 + (P4 << 1);
	R2 = W[P0] (Z);
	IF CC R2 = R4;
	W[P3 + 4] = R2;
	CC = R3 == R5;
	R4 = W[P3 + 6] (Z);
	R2 = W[P4] (Z);
	IF CC R2 = R4;
	W[P3 + 6] = R2;
	R2 = 0x0808 (Z);
	R5 = 0x1008 (Z);
	JUMP .Lw_end;
	.size _spike_index_key_row, .-_spike_index_key_row
