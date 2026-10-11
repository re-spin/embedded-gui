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
    40  u32  narrow   w - 1 for a one-byte block (skip + w <= 8, w >= 2),
                      with shift = 24 + skip and no keep or src2; else 0

   Blank rows are skipped, and a row loops only from its first set bit to
   its last (clz(x) = SIGNBITS(x >> 1) for a nonzero x below 2^31 after the
   shift).  Rows are the outer hardware loop; a blank row jumps to its last
   instruction.  ROT through CC hands over one bit per pixel; the pixel loop
   is software-pipelined, so a pixel is a conditional move and one
   multi-issue of the next rotate, its store and the next pixel's load
   (through I0, one pixel ahead): 2 cycles.  The last pixel of a span is
   always set and is a plain store, so no load goes past it.  The
   multi-issue stores R0 as it was before the parallel load writes R0.L
   (the simulator agrees: frames match the host).  Writes R0-R3,
   P0-P2, I0, LC0/1, LT0/1, LB0/1, CC; saves R4-R7, P3-P5.

   _spike_mask1_glyphs(const struct GlyphRun *r, unsigned n): n >= 1 glyphs
   in one call, each clipped and set up here.  The Rust wrapper
   (firmware::mask1_batch) checks the clip once (inside px, below 2^15) and
   each glyph's mask (A1, 1..16 px wide, h and sstep below 2^15, every row
   inside its data, x and y within +-2^30), so no block can leave px or
   its mask.

   GlyphRun:  0 u16 *px  4 u32 stride (px)  8 i32 cx  12 cy  16 cr  20 cb
              (clip right and bottom exclusive)  24 u32 ink
              28 const Glyph *g
   Glyph:     0 i32 x  4 i32 y  8 u32 w  12 u32 h  16 u32 sstep  20 u8 *data
              (24 bytes)

   The run's constants live in B0-B3 (clip), M0 (ink), M2 (px), M3
   (stride), I2 (dstep) and I1 (next glyph), so the row bodies keep all of
   R0-R6 and P0-P5.  Also writes I1-I2, M0-M3, B0-B3 (L0-L3 stay 0).  */
/* Fixed-width rows of a one-byte block (2..8 px): P5 dst, P3 dstep, P1 src,
   P4 sstep, R5 shift (24 + skip), R4 rows, R2 ink, P2 w - 1.  No first/last
   search and no second byte: ~10 + 2w cycles a row.  */
	.macro NARROW_BODY
	LC1 = R4;
	LSETUP (.Lnrow\@, .Lnend\@) LC1;
.Lnrow\@:
	P0 = P5;
	I0 = P5;
	R1 = B[P1] (Z);
	P1 = P1 + P4;
	R1 = LSHIFT R1 BY R5.L;
	R1 = ROT R1 BY 1 || R0.L = W[I0++] || NOP;
	LSETUP (.Ln0\@, .Ln1\@) LC0 = P2;
.Ln0\@:
	IF CC R0 = R2;
.Ln1\@:
	R1 = ROT R1 BY 1 || R0.L = W[I0++] || W[P0++] = R0;
	IF CC R0 = R2;
	W[P0] = R0;
.Lnend\@:
	P5 = P5 + P3;
	.endm

/* Span rows: P5 dst, P3 dstep, P1 src, P2 src2, P4 sstep, R5 shift
   (16 + skip), R6 keep, R4 rows, R2 ink.  */
	.macro WIDE_BODY
	LC1 = R4;
	LSETUP (.Lrow\@, .Lnext\@) LC1;
.Lrow\@:
	R0 = B[P1] (Z);
	R1 = B[P2] (Z);
	R0 <<= 8;
	R0 = R0 | R1;
	R1 = LSHIFT R0 BY R5.L;
	R1 = R1 & R6;
	P1 = P1 + P4;
	P2 = P2 + P4;
	CC = R1 == 0;
	IF CC JUMP .Lnext\@;
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
	R3 += -1;
	CC = R3 == 0;
	P0 = P5 + (P0 << 1);
	IF CC JUMP .Llast\@;
	LC0 = R3;
	I0 = P0;
	R1 = ROT R1 BY 1 || R0 = W[P0] (Z) || NOP;
	I0 += 2;
	LSETUP (.Lp0\@, .Lp1\@) LC0;
.Lp0\@:
	IF CC R0 = R2;
.Lp1\@:
	R1 = ROT R1 BY 1 || R0.L = W[I0++] || W[P0++] = R0;
.Llast\@:
	W[P0] = R2;
.Lnext\@:
	P5 = P5 + P3;
	.endm

	.section .text.spike_mask1_rows,"ax",@progbits
	.align 4
	.global _spike_mask1_rows
	.type _spike_mask1_rows, STT_FUNC
_spike_mask1_rows:
	[--SP] = (R7:4, P5:3);
	P2 = R0;
	R3 = [P2 + 40];
	P5 = [P2];
	P3 = [P2 + 4];
	P1 = [P2 + 8];
	P4 = [P2 + 12];
	R5 = [P2 + 16];
	R6 = [P2 + 20];
	R4 = [P2 + 28];
	R2 = [P2 + 32];
	CC = R3 == 0;
	IF CC JUMP .Lrows_wide;
	P2 = R3;
	NARROW_BODY
	JUMP .Lrows_done;
.Lrows_wide:
	P2 = [P2 + 36];
	WIDE_BODY
.Lrows_done:
	(R7:4, P5:3) = [SP++];
	RTS;
	.size _spike_mask1_rows, .-_spike_mask1_rows

	.global _spike_mask1_glyphs
	.type _spike_mask1_glyphs, STT_FUNC
_spike_mask1_glyphs:
	[--SP] = (R7:4, P5:3);
	P0 = R0;
	R7 = R1;
	R0 = [P0];
	M2 = R0;
	R0 = [P0 + 4];
	M3 = R0;
	R0 <<= 1;
	I2 = R0;
	R0 = [P0 + 8];
	B0 = R0;
	R0 = [P0 + 12];
	B1 = R0;
	R0 = [P0 + 16];
	B2 = R0;
	R0 = [P0 + 20];
	B3 = R0;
	R0 = [P0 + 24];
	M0 = R0;
	R0 = [P0 + 28];
	I1 = R0;
	R0 = 24;
	M1 = R0;
.Lg:
	P0 = I1;
	I1 += M1;
	R0 = [P0];		/* x */
	R1 = [P0 + 4];		/* y */
	R3 = [P0 + 8];		/* w */
	R4 = [P0 + 12];		/* h */
	R5 = B0;
	R5 = MAX (R0, R5);	/* x0 */
	R3 = R3 + R0;
	R6 = B2;
	R3 = MIN (R3, R6);	/* x1 */
	CC = R5 < R3;
	IF !CC JUMP .Lgnext;
	R6 = B1;
	R6 = MAX (R1, R6);	/* y0 */
	R4 = R4 + R1;
	R2 = B3;
	R4 = MIN (R4, R2);	/* y1 */
	CC = R6 < R4;
	IF !CC JUMP .Lgnext;
	R3 = R3 - R5;		/* clipped w */
	R4 = R4 - R6;		/* clipped h */
	R0 = R5 - R0;		/* columns skipped */
	R1 = R6 - R1;		/* rows skipped */
	R2 = M3;
	R6 *= R2;
	R6 = R6 + R5;
	R6 <<= 1;
	R2 = M2;
	R6 = R6 + R2;
	P5 = R6;		/* dst */
	P4 = [P0 + 16];
	R2 = [P0 + 16];
	R1 *= R2;
	R2 = [P0 + 20];
	R1 = R1 + R2;
	P1 = R1;		/* src */
	P3 = I2;
	R2 = M0;
	R6 = R0 + R3;
	R1 = 8;
	CC = R6 <= R1;
	IF !CC JUMP .Lgtwo;
	CC = R3 == 1;
	IF CC JUMP .Lgone;
	R5 = R0;
	R5 += 24;
	R3 += -1;
	P2 = R3;
	NARROW_BODY
	JUMP .Lgnext;
.Lgone:
	P2 = P1;		/* one column: src2 = src */
	JUMP .Lgwide;
.Lgtwo:
	P2 = P1;
	P2 += 1;
.Lgwide:
	R5 = R0;
	R5 += 16;
	R1 = 32;
	R1 = R1 - R3;
	R6 = -1;
	R6 <<= R1;
	WIDE_BODY
.Lgnext:
	R7 += -1;
	CC = R7 == 0;
	IF !CC JUMP .Lg (BP);
	(R7:4, P5:3) = [SP++];
	RTS;
	.size _spike_mask1_glyphs, .-_spike_mask1_glyphs
