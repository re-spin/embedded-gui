/* _spike_mask1_rows(const struct Mask1Args *a): an A1 mask up to 16 px
   wide, C ABI.  The Rust wrapper (spikes/bfin/src/lib.rs,
   firmware::mask1_rows) bounds-checks the block and hands over:

     0  u16 *dst      top-left pixel
     4  int  dstep    bytes between rows
     8  u8  *src      first mask byte of the top row
    36  u8  *src2     src + 1, or src for 1-byte rows (w + skipped <= 8), whose
                      repeated byte lands below the kept bits
    12  int  sstep    bytes between mask rows
    16  u32  shift    16 + columns skipped on the left (moves column 0 to bit 31)
    20  u32  keep     the top w bits set
    24  u32  w        pixels a row, >= 1
    28  u32  h        rows, >= 1
    32  u32  ink

   Blank rows are skipped, and a row loops only from its first set bit to
   its last (clz(x) = SIGNBITS(x >> 1) for a nonzero x below 2^31 after the
   shift).  Rows are the outer hardware loop; a blank row jumps to its last
   instruction.  ROT through CC hands over one bit per pixel, so a pixel is a load
   issued with the rotate, a conditional move and a store: 3 cycles.  Writes
   R0-R3, P0-P2, LC0/1, LT0/1, LB0/1, CC; saves R4-R7, P3-P5.  */
	.section .text.spike_mask1_rows,"ax",@progbits
	.align 4
	.global _spike_mask1_rows
	.type _spike_mask1_rows, STT_FUNC
_spike_mask1_rows:
	[--SP] = (R7:4, P5:3);
	P2 = R0;
	P5 = [P2];
	P3 = [P2 + 4];
	P1 = [P2 + 8];
	P4 = [P2 + 12];
	R5 = [P2 + 16];
	R6 = [P2 + 20];
	R4 = [P2 + 28];
	R2 = [P2 + 32];
	P2 = [P2 + 36];
	LC1 = R4;
	LSETUP (.Lrow, .Lnext) LC1;
.Lrow:
	R0 = B[P1] (Z);
	R1 = B[P2] (Z);
	R0 <<= 8;
	R0 = R0 | R1;
	R1 = LSHIFT R0 BY R5.L;
	R1 = R1 & R6;
	P1 = P1 + P4;
	P2 = P2 + P4;
	CC = R1 == 0;
	IF CC JUMP .Lnext;
	/* R0 = first set pixel, R3 = last - first + 1 */
	R0 = R1 >> 1;
	R0.L = SIGNBITS R0;
	R0 = R0.L (Z);
	R3 = -R1;
	R3 = R3 & R1;
	R3 >>= 1;
	R3.L = SIGNBITS R3;
	R3 = R3.L (Z);
	R3 = R3 - R0;
	R3 += 1;
	R1 = LSHIFT R1 BY R0.L;
	P0 = R0;
	LC0 = R3;
	P0 = P5 + (P0 << 1);
	LSETUP (.Lp0, .Lp1) LC0;
.Lp0:
	R1 = ROT R1 BY 1 || R0 = W[P0] (Z) || NOP;
	IF CC R0 = R2;
.Lp1:
	W[P0++] = R0;
.Lnext:
	P5 = P5 + P3;
	(R7:4, P5:3) = [SP++];
	RTS;
	.size _spike_mask1_rows, .-_spike_mask1_rows
