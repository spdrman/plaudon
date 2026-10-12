//! Unicode Normalization Form C, implemented against the generated tables in
//! [`super::nfc_tables`].
//!
//! Why this is here rather than behind a dependency: the only Unicode operation
//! `plaudon-security` needs is "put a filename into one canonical form so two
//! names that are canonically equivalent cannot become two different files in
//! the managed archive" (§13.4). That is UAX #15 §1.3 plus the composition
//! algorithm of UAX #15 §X, roughly 100 lines over three sorted tables, and it
//! keeps the security boundary free of a transitive dependency.
//!
//! The algorithm is the standard one and is not approximated:
//!
//! 1. recursive canonical decomposition (Hangul arithmetically, everything else
//!    from `DECOMP_*`);
//! 2. canonical ordering — a stable sort of each combining run by canonical
//!    combining class;
//! 3. canonical composition — pair the last starter with each following
//!    character when it is not blocked, using primary composites only, so the
//!    Composition Exclusion Table is honoured.
//!
//! # Regenerating the tables
//!
//! ```text
//! python3 - <<'EOF'
//! import unicodedata as U
//! HANGUL_S = range(0xAC00, 0xD7A4)
//! decomp, ccc, comp = [], [], []
//! for cp in range(0x110000):
//!     c = chr(cp)
//!     k = U.combining(c)
//!     if k:
//!         ccc.append((cp, k))
//!     if cp in HANGUL_S:
//!         continue
//!     d = U.decomposition(c)
//!     if d and not d.startswith('<'):
//!         parts = [int(x, 16) for x in d.split()]
//!         a = parts[0]; b = parts[1] if len(parts) == 2 else 0
//!         decomp.append((cp, a, b))
//!         if b and U.normalize('NFC', chr(a) + chr(b)) == c:
//!             comp.append((a, b, cp))
//! comp.sort(key=lambda t: (t[0], t[1]))
//! # emit DECOMP_CP/DECOMP_A/DECOMP_B, COMPOSE_PAIR/COMPOSE_RESULT,
//! # CCC_CP/CCC_CLASS as sorted static slices
//! EOF
//! ```
//!
//! `COMPOSE_PAIR` is derived through `unicodedata.normalize`, so a pair that
//! Unicode excludes from composition simply never enters the table.

use super::nfc_tables::{
    CCC_CLASS, CCC_CP, COMPOSE_PAIR, COMPOSE_RESULT, DECOMP_A, DECOMP_B, DECOMP_CP,
};

const S_BASE: u32 = 0xAC00;
const L_BASE: u32 = 0x1100;
const V_BASE: u32 = 0x1161;
const T_BASE: u32 = 0x11A7;
const L_COUNT: u32 = 19;
const V_COUNT: u32 = 21;
const T_COUNT: u32 = 28;
const N_COUNT: u32 = V_COUNT * T_COUNT;
const S_COUNT: u32 = L_COUNT * N_COUNT;

/// Canonical combining class of `ch`; 0 for a starter.
pub fn combining_class(ch: char) -> u8 {
    let cp = ch as u32;
    if cp < 0x0300 {
        // No character below U+0300 has a non-zero combining class.
        return 0;
    }
    CCC_CP
        .binary_search(&cp)
        .map_or(0, |index| CCC_CLASS[index])
}

/// Canonical decomposition of `ch`, excluding Hangul (handled arithmetically).
fn canonical_decomposition(ch: char) -> Option<(char, Option<char>)> {
    let index = DECOMP_CP.binary_search(&(ch as u32)).ok()?;
    let first = char::from_u32(DECOMP_A[index])?;
    let second = if DECOMP_B[index] == 0 {
        None
    } else {
        char::from_u32(DECOMP_B[index])
    };
    Some((first, second))
}

/// The primary composite of `first` and `second`, if there is one.
fn primary_composite(first: char, second: char) -> Option<char> {
    let (a, b) = (first as u32, second as u32);

    // Hangul: leading jamo + vowel jamo.
    if (L_BASE..L_BASE + L_COUNT).contains(&a) && (V_BASE..V_BASE + V_COUNT).contains(&b) {
        let index = (a - L_BASE) * N_COUNT + (b - V_BASE) * T_COUNT;
        return char::from_u32(S_BASE + index);
    }
    // Hangul: LV syllable + trailing jamo.
    if (S_BASE..S_BASE + S_COUNT).contains(&a)
        && (a - S_BASE).is_multiple_of(T_COUNT)
        && (T_BASE + 1..T_BASE + T_COUNT).contains(&b)
    {
        return char::from_u32(a + (b - T_BASE));
    }

    let key = (u64::from(a) << 32) | u64::from(b);
    let index = COMPOSE_PAIR.binary_search(&key).ok()?;
    char::from_u32(COMPOSE_RESULT[index])
}

fn decompose_into(ch: char, out: &mut Vec<char>) {
    let cp = ch as u32;
    if (S_BASE..S_BASE + S_COUNT).contains(&cp) {
        let index = cp - S_BASE;
        let (l, v, t) = (
            index / N_COUNT,
            (index % N_COUNT) / T_COUNT,
            index % T_COUNT,
        );
        out.extend(char::from_u32(L_BASE + l));
        out.extend(char::from_u32(V_BASE + v));
        if t != 0 {
            out.extend(char::from_u32(T_BASE + t));
        }
        return;
    }
    match canonical_decomposition(ch) {
        Some((first, second)) => {
            decompose_into(first, out);
            if let Some(second) = second {
                decompose_into(second, out);
            }
        }
        None => out.push(ch),
    }
}

/// Stable sort of each combining run by canonical combining class (UAX #15).
fn canonical_order(buffer: &mut [char]) {
    for i in 1..buffer.len() {
        let class = combining_class(buffer[i]);
        if class == 0 {
            continue;
        }
        let mut j = i;
        while j > 0 && combining_class(buffer[j - 1]) > class {
            buffer.swap(j - 1, j);
            j -= 1;
        }
    }
}

/// Normalise `input` to NFC.
pub fn to_nfc(input: &str) -> String {
    if input.is_ascii() {
        return input.to_owned();
    }

    let mut decomposed: Vec<char> = Vec::with_capacity(input.len());
    for ch in input.chars() {
        decompose_into(ch, &mut decomposed);
    }
    canonical_order(&mut decomposed);

    let mut out: Vec<char> = Vec::with_capacity(decomposed.len());
    let mut last_starter: Option<usize> = None;
    let mut previous_class = 0u8;

    for ch in decomposed {
        let class = combining_class(ch);
        if let Some(starter) = last_starter {
            // Not blocked when nothing stands between the starter and `ch`, or
            // when the preceding character's class is strictly lower (the run
            // is already canonically ordered, so that implies no intervening
            // character has a class >= `class`, and none is a starter).
            // (`previous_class < class` is already false for a starter, which
            // is exactly the "blocked by an intervening character" case.)
            let adjacent = out.len() == starter + 1;
            if adjacent || previous_class < class {
                if let Some(composite) = primary_composite(out[starter], ch) {
                    out[starter] = composite;
                    continue;
                }
            }
        }
        if class == 0 {
            last_starter = Some(out.len());
        }
        previous_class = class;
        out.push(ch);
    }

    out.into_iter().collect()
}

/// Whether `input` is already in NFC.
pub fn is_nfc(input: &str) -> bool {
    input.is_ascii() || to_nfc(input) == input
}
