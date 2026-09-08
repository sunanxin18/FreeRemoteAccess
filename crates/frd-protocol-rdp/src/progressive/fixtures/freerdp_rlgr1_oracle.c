/**
 * FreeRDP: A Remote Desktop Protocol Implementation
 * RemoteFX Codec Library - RLGR
 *
 * Copyright 2011 Vic Lee
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *     http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

/**
 * This implementation of RLGR refers to
 * [MS-RDPRFX] 3.1.8.1.7.3 RLGR1/RLGR3 Pseudocode
 */

/* Test-only extraction from FreeRDP 54a873e2710710841c6ec2b756df64285ec6e29a
 * libfreerdp/codec/rfx_rlgr.c. Encoder functions unchanged; platform allocation,
 * casts and bitstream replaced by bounded local shims; synthetic main added. */
#include <stdint.h>
#include <stdlib.h>
#include <stdio.h>
#include <assert.h>
#include <string.h>
typedef uint32_t UINT32; typedef int32_t INT32; typedef int16_t INT16; typedef uint16_t UINT16; typedef uint8_t BYTE; typedef uint8_t UINT8; typedef int RLGR_MODE;
#define RLGR1 1
#define WINPR_RESTRICT
#define WINPR_ASSERT assert
#define WINPR_ASSERTING_INT_CAST(t,v) ((t)(v))
#define WINPR_CXX_COMPAT_CAST(t,v) ((t)(v))
#define winpr_aligned_calloc(n,s,a) calloc(n,s)
#define winpr_aligned_free free
typedef struct { BYTE* data; size_t bits,cap; } RFX_BITSTREAM;
void rfx_bitstream_attach(RFX_BITSTREAM*b,BYTE*d,size_t n){b->data=d;b->bits=0;b->cap=n*8;}
void rfx_bitstream_put_bits(RFX_BITSTREAM*b,UINT32 v,UINT32 n){assert(b->bits+n<=b->cap);for(UINT32 j=n;j>0;--j){size_t p=b->bits++;b->data[p/8]|=((v>>(j-1))&1)<<(7-p%8);}}
void rfx_bitstream_flush(RFX_BITSTREAM*b){(void)b;}
UINT32 rfx_bitstream_get_processed_bytes(RFX_BITSTREAM*b){return (b->bits+7)/8;}
#define KPMAX (80) /* max value for kp or krp */
#define LSGR (3)   /* shift count to convert kp to k */
#define UP_GR (4)  /* increase in kp after a zero run in RL mode */
#define DN_GR (6)  /* decrease in kp after a nonzero symbol in RL mode */
#define UQ_GR (3)  /* increase in kp after nonzero symbol in GR mode */
#define DQ_GR (3)  /* decrease in kp after zero symbol in GR mode */

/* Returns the least number of bits required to represent a given value */
#define GetMinBits(_val, _nbits) \
	do                           \
	{                            \
		UINT32 _v = (_val);      \
		(_nbits) = 0;            \
		while (_v)               \
		{                        \
			_v >>= 1;            \
			(_nbits)++;          \
		}                        \
	} while (0)

/*
 * Update the passed parameter and clamp it to the range [0, KPMAX]
 * Return the value of parameter right-shifted by LSGR
 */
static inline uint32_t UpdateParam(uint32_t* param, int32_t deltaP)
{
	WINPR_ASSERT(param);
	if (deltaP < 0)
	{
		const uint32_t udeltaP = WINPR_ASSERTING_INT_CAST(uint32_t, -deltaP);
		if (udeltaP > *param)
			*param = 0;
		else
			*param -= udeltaP;
	}
	else
		*param += WINPR_ASSERTING_INT_CAST(uint32_t, deltaP);

	if ((*param) > KPMAX)
		(*param) = KPMAX;
	return (*param) >> LSGR;
}/* Returns the next coefficient (a signed int) to encode, from the input stream */
#define GetNextInput(_n)    \
	do                      \
	{                       \
		if (data_size > 0)  \
		{                   \
			(_n) = *data++; \
			data_size--;    \
		}                   \
		else                \
		{                   \
			(_n) = 0;       \
		}                   \
	} while (0)

/* Emit bitPattern to the output bitstream */
#define OutputBits(numBits, bitPattern) rfx_bitstream_put_bits(bs, bitPattern, numBits)

/* Emit a bit (0 or 1), count number of times, to the output bitstream */
static inline void OutputBit(RFX_BITSTREAM* bs, uint32_t count, UINT8 bit)
{
	UINT16 _b = ((bit) ? 0xFFFF : 0);
	const uint32_t rem = count % 16;
	for (uint32_t x = 0; x < count - rem; x += 16)
		rfx_bitstream_put_bits(bs, _b, 16);

	if (rem > 0)
		rfx_bitstream_put_bits(bs, _b, rem);
}

/* Converts the input value to (2 * abs(input) - sign(input)), where sign(input) = (input < 0 ? 1 :
 * 0) and returns it */
static inline UINT32 Get2MagSign(INT32 input)
{
	if (input >= 0)
		return WINPR_ASSERTING_INT_CAST(UINT32, 2 * input);
	return WINPR_ASSERTING_INT_CAST(UINT32, -2 * input - 1);
}

/* Outputs the Golomb/Rice encoding of a non-negative integer */
#define CodeGR(krp, val) rfx_rlgr_code_gr(bs, krp, val)

static void rfx_rlgr_code_gr(RFX_BITSTREAM* bs, uint32_t* krp, UINT32 val)
{
	uint32_t kr = *krp >> LSGR;

	/* unary part of GR code */

	const uint32_t vk = val >> kr;
	OutputBit(bs, vk, 1);
	OutputBit(bs, 1, 0);

	/* remainder part of GR code, if needed */
	if (kr)
	{
		OutputBits(kr, val & ((1u << kr) - 1));
	}

	/* update krp, only if it is not equal to 1 */
	if (vk == 0)
	{
		(void)UpdateParam(krp, -2);
	}
	else if (vk > 1)
	{
		(void)UpdateParam(krp, WINPR_CXX_COMPAT_CAST(int32_t, vk));
	}
}

int rfx_rlgr_encode(RLGR_MODE mode, const INT16* WINPR_RESTRICT data, UINT32 data_size,
                    BYTE* WINPR_RESTRICT buffer, UINT32 buffer_size)
{
	RFX_BITSTREAM* bs = (RFX_BITSTREAM*)winpr_aligned_calloc(1, sizeof(RFX_BITSTREAM), 32);

	if (!bs)
		return 0;

	rfx_bitstream_attach(bs, buffer, buffer_size);

	/* initialize the parameters */
	uint32_t k = 1;
	uint32_t kp = 1u << LSGR;
	uint32_t krp = 1u << LSGR;

	/* process all the input coefficients */
	while (data_size > 0)
	{
		int input = 0;

		if (k)
		{
			uint32_t numZeros = 0;
			uint32_t runmax = 0;
			BYTE sign = 0;

			/* RUN-LENGTH MODE */

			/* collect the run of zeros in the input stream */
			numZeros = 0;
			GetNextInput(input);
			while (input == 0 && data_size > 0)
			{
				numZeros++;
				GetNextInput(input);
			}

			// emit output zeros
			runmax = 1u << k;
			while (numZeros >= runmax)
			{
				OutputBit(bs, 1, 0); /* output a zero bit */
				numZeros -= runmax;
				k = UpdateParam(&kp, UP_GR); /* update kp, k */
				runmax = 1u << k;
			}

			/* output a 1 to terminate runs */
			OutputBit(bs, 1, 1);

			/* output the remaining run length using k bits */
			OutputBits(k, numZeros);

			/* note: when we reach here and the last byte being encoded is 0, we still
			   need to output the last two bits, otherwise mstsc will crash */

			/* encode the nonzero value using GR coding */
			const UINT32 mag =
			    (UINT32)(input < 0 ? -input : input); /* absolute value of input coefficient */
			sign = (input < 0 ? 1 : 0);         /* sign of input coefficient */

			OutputBit(bs, 1, sign);          /* output the sign bit */
			CodeGR(&krp, mag ? mag - 1 : 0); /* output GR code for (mag - 1) */

			k = UpdateParam(&kp, -DN_GR);
		}
		else
		{
			/* GOLOMB-RICE MODE */

			if (mode == RLGR1)
			{
				UINT32 twoMs = 0;

				/* RLGR1 variant */

				/* convert input to (2*magnitude - sign), encode using GR code */
				GetNextInput(input);
				twoMs = Get2MagSign(input);
				CodeGR(&krp, twoMs);

				/* update k, kp */
				/* NOTE: as of Aug 2011, the algorithm is still wrongly documented
				   and the update direction is reversed */
				if (twoMs)
				{
					k = UpdateParam(&kp, -DQ_GR);
				}
				else
				{
					k = UpdateParam(&kp, UQ_GR);
				}
			}
			else /* mode == RLGR3 */
			{
				UINT32 twoMs1 = 0;
				UINT32 twoMs2 = 0;
				UINT32 sum2Ms = 0;
				UINT32 nIdx = 0;

				/* RLGR3 variant */

				/* convert the next two input values to (2*magnitude - sign) and */
				/* encode their sum using GR code */

				GetNextInput(input);
				twoMs1 = Get2MagSign(input);
				GetNextInput(input);
				twoMs2 = Get2MagSign(input);
				sum2Ms = twoMs1 + twoMs2;

				CodeGR(&krp, sum2Ms);

				/* encode binary representation of the first input (twoMs1). */
				GetMinBits(sum2Ms, nIdx);
				OutputBits(nIdx, twoMs1);

				/* update k,kp for the two input values */

				if (twoMs1 && twoMs2)
				{
					k = UpdateParam(&kp, -2 * DQ_GR);
				}
				else if (!twoMs1 && !twoMs2)
				{
					k = UpdateParam(&kp, 2 * UQ_GR);
				}
			}
		}
	}

	rfx_bitstream_flush(bs);
	uint32_t processed_size = rfx_bitstream_get_processed_bytes(bs);
	winpr_aligned_free(bs);

	return WINPR_ASSERTING_INT_CAST(int, processed_size);
}

int main(int argc,char**argv){INT16 c[4096];BYTE b[65536]={0};for(int i=0;i<4096;i++)c[i]=(i<4000)?(INT16)((i%17)-8):0; c[4095]=1;int n=rfx_rlgr_encode(RLGR1,c,4096,b,sizeof b);FILE*f=fopen(argv[1],"wb");fwrite(b,1,n,f);fclose(f);printf("%d\n",n);}
