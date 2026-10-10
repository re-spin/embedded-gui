/* _spike_index_key_row(u16 *dst, const u8 *src, unsigned n, const u16 *lut):
   one indexed8 row with transparent index 255, C ABI (lut on the stack).
   dst[i] = lut[src[i]] unless src[i] == 255.

   Pixels one at a time until src is 4-aligned, then a word of four indices
   at a time: an all-255 word skips its four pixels on one compare (a
   transparent run); otherwise two pairs, each index's table address one
   instruction after its move so the DAG does not stall.  A kept pixel keeps
   its old value through a conditional move, so the stores need no branch.
   The skip jumps to the word loop's last instruction, as mask1.S does.
   Then the 0-3 pixels left.  Writes R0-R3, P0-P2, LC0, LT0, LB0, CC;
   saves R4-R7, P3-P5.

   For sprites (transparent runs).  4d0ec62 had a branch-free path for
   words with no 255 (7.7/px on a nearly opaque image vs 10.1 here) but cost
   more on mixed words; nearly opaque images are better as Rgb555.  */
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
	R7 = -1;
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
	CC = R3 == 0;
	IF CC JUMP .Ltail;
	P5 = R3;
	LSETUP (.Lw, .Lw_end) LC0 = P5;
.Lw:
	R0 = [P1++];
	CC = R0 == R7;
	IF CC JUMP .Lw_end;
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
.Lw_end:
	P3 += 8;
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
	.size _spike_index_key_row, .-_spike_index_key_row
