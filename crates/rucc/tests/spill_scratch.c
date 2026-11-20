/* slot_deform_heap_tuple from Postgres REL_18_6, cut down to what tts_buffer_heap_getsomeattrs needs. It spills, and spill_scratch.rs reads which registers it was given. */
typedef unsigned long Datum;
typedef unsigned long size_t;
typedef unsigned long uintptr_t;
typedef unsigned char bits8;
typedef unsigned char uint8;
typedef unsigned short uint16;
typedef unsigned int uint32;
typedef short int16;
typedef int int32;
#if __STDC_VERSION__ < 202311L
typedef _Bool bool;
#define true 1
#define false 0
#endif
size_t strlen(const char *);

typedef struct CompactAttribute {
  int32 attcacheoff;
  int16 attlen;
  bool attbyval, attispackable, atthasmissing, attisdropped, attgenerated;
  char attnullability;
  uint8 attalignby;
} CompactAttribute;

typedef struct TupleDescData {
  int natts;
  unsigned tdtypeid;
  int tdtypmod;
  int tdrefcount;
  void *constr;
  CompactAttribute compact_attrs[];
} *TupleDesc;

typedef struct HeapTupleHeaderData {
  char t_choice[12];
  char t_ctid[6];
  uint16 t_infomask2;
  uint16 t_infomask;
  uint8 t_hoff;
  bits8 t_bits[];
} HeapTupleHeaderData, *HeapTupleHeader;

typedef struct HeapTupleData {
  uint32 t_len;
  char t_self[6];
  unsigned t_tableOid;
  HeapTupleHeader t_data;
} HeapTupleData, *HeapTuple;

typedef struct TupleTableSlot {
  int type;
  uint16 tts_flags;
  int16 tts_nvalid;
  const void *tts_ops;
  TupleDesc tts_tupleDescriptor;
  Datum *tts_values;
  bool *tts_isnull;
  void *tts_mcxt;
  char tts_tid[6];
  unsigned tts_tableOid;
} TupleTableSlot;

typedef struct BufferHeapTupleTableSlot {
  TupleTableSlot base;
  HeapTuple tuple;
  uint32 off;
  HeapTupleData tupdata;
  int buffer;
} BufferHeapTupleTableSlot;

#define TTS_FLAG_SLOW (1 << 3)
#define TTS_SLOW(slot) (((slot)->tts_flags & TTS_FLAG_SLOW) != 0)
#define HEAP_HASNULL 0x0001
#define HEAP_NATTS_MASK 0x07FF
#define HeapTupleHasNulls(tuple) (((tuple)->t_data->t_infomask & HEAP_HASNULL) != 0)
#define HeapTupleHeaderGetNatts(tup) ((tup)->t_infomask2 & HEAP_NATTS_MASK)
#define Min(x, y) ((x) < (y) ? (x) : (y))
#define TYPEALIGN(ALIGNVAL, LEN) (((uintptr_t) (LEN) + ((ALIGNVAL) - 1)) & ~((uintptr_t) ((ALIGNVAL) - 1)))
#define VARATT_IS_1B(PTR) ((((const uint8 *) (PTR))[0] & 0x01) == 0x01)
#define VARATT_IS_1B_E(PTR) ((((const uint8 *) (PTR))[0]) == 0x01)
#define VARATT_NOT_PAD_BYTE(PTR) (*((const uint8 *) (PTR)) != 0)
#define VARSIZE_4B(PTR) ((*(const uint32 *) (PTR) >> 2) & 0x3FFFFFFF)
#define VARSIZE_1B(PTR) ((((const uint8 *) (PTR))[0] >> 1) & 0x7F)
#define VARSIZE_EXTERNAL(PTR) (2 + (((const uint8 *) (PTR))[1] == 18 ? 16 : 10))
#define VARSIZE_ANY(PTR) \
  (VARATT_IS_1B_E(PTR) ? VARSIZE_EXTERNAL(PTR) : (VARATT_IS_1B(PTR) ? VARSIZE_1B(PTR) : VARSIZE_4B(PTR)))
#define att_pointer_alignby(cur_offset, attalignby, attlen, attptr) \
  (((attlen) == -1 && VARATT_NOT_PAD_BYTE(attptr)) ? (uintptr_t) (cur_offset) : TYPEALIGN(attalignby, cur_offset))
#define att_nominal_alignby(cur_offset, attalignby) TYPEALIGN(attalignby, cur_offset)
#define att_addlength_pointer(cur_offset, attlen, attptr) \
  (((attlen) > 0) ? ((cur_offset) + (attlen)) \
   : (((attlen) == -1) ? ((cur_offset) + VARSIZE_ANY(attptr)) : ((cur_offset) + (strlen((char *) (attptr)) + 1))))
#define fetchatt(A, T) fetch_att(T, (A)->attbyval, (A)->attlen)

static inline CompactAttribute *TupleDescCompactAttr(TupleDesc tupdesc, int i) {
  return &tupdesc->compact_attrs[i];
}

static inline bool att_isnull(int ATT, const bits8 *BITS) {
  return !(BITS[ATT >> 3] & (1 << (ATT & 0x07)));
}

static inline Datum fetch_att(const void *T, bool attbyval, int attlen) {
  if (attbyval) {
    switch (attlen) {
    case sizeof(char): return (Datum) *((const char *) T);
    case sizeof(int16): return (Datum) *((const int16 *) T);
    case sizeof(int32): return (Datum) *((const int32 *) T);
    case sizeof(Datum): return *((const Datum *) T);
    default: return 0;
    }
  }
  return (Datum) T;
}

static inline __attribute__((always_inline)) int
slot_deform_heap_tuple_internal(TupleTableSlot *slot, HeapTuple tuple, int attnum, int natts, bool slow,
                                bool hasnulls, uint32 *offp, bool *slowp) {
  TupleDesc tupleDesc = slot->tts_tupleDescriptor;
  Datum *values = slot->tts_values;
  bool *isnull = slot->tts_isnull;
  HeapTupleHeader tup = tuple->t_data;
  char *tp;
  bits8 *bp = tup->t_bits;
  bool slownext = false;

  tp = (char *) tup + tup->t_hoff;

  for (; attnum < natts; attnum++) {
    CompactAttribute *thisatt = TupleDescCompactAttr(tupleDesc, attnum);

    if (hasnulls && att_isnull(attnum, bp)) {
      values[attnum] = (Datum) 0;
      isnull[attnum] = true;
      if (!slow) {
        *slowp = true;
        return attnum + 1;
      } else
        continue;
    }

    isnull[attnum] = false;

    if (!slow && thisatt->attcacheoff >= 0)
      *offp = thisatt->attcacheoff;
    else if (thisatt->attlen == -1) {
      if (!slow && *offp == att_nominal_alignby(*offp, thisatt->attalignby))
        thisatt->attcacheoff = *offp;
      else {
        *offp = att_pointer_alignby(*offp, thisatt->attalignby, -1, tp + *offp);
        if (!slow)
          slownext = true;
      }
    } else {
      *offp = att_nominal_alignby(*offp, thisatt->attalignby);
      if (!slow)
        thisatt->attcacheoff = *offp;
    }

    values[attnum] = fetchatt(thisatt, tp + *offp);

    *offp = att_addlength_pointer(*offp, thisatt->attlen, tp + *offp);

    if (!slow) {
      if (slownext || thisatt->attlen <= 0) {
        *slowp = true;
        return attnum + 1;
      }
    }
  }

  return natts;
}

static inline __attribute__((always_inline)) void
slot_deform_heap_tuple(TupleTableSlot *slot, HeapTuple tuple, uint32 *offp, int natts) {
  bool hasnulls = HeapTupleHasNulls(tuple);
  int attnum;
  uint32 off;
  bool slow;

  natts = Min(HeapTupleHeaderGetNatts(tuple->t_data), natts);

  attnum = slot->tts_nvalid;
  if (attnum == 0) {
    off = 0;
    slow = false;
  } else {
    off = *offp;
    slow = TTS_SLOW(slot);
  }

  if (!slow) {
    if (!hasnulls)
      attnum = slot_deform_heap_tuple_internal(slot, tuple, attnum, natts, false, false, &off, &slow);
    else
      attnum = slot_deform_heap_tuple_internal(slot, tuple, attnum, natts, false, true, &off, &slow);
  }

  if (attnum < natts) {
    attnum = slot_deform_heap_tuple_internal(slot, tuple, attnum, natts, true, hasnulls, &off, &slow);
  }

  slot->tts_nvalid = attnum;
  *offp = off;
  if (slow)
    slot->tts_flags |= TTS_FLAG_SLOW;
  else
    slot->tts_flags &= ~TTS_FLAG_SLOW;
}

void tts_buffer_heap_getsomeattrs(TupleTableSlot *slot, int natts) {
  BufferHeapTupleTableSlot *bslot = (BufferHeapTupleTableSlot *) slot;
  slot_deform_heap_tuple(slot, bslot->tuple, &bslot->off, natts);
}
