use core::borrow::Borrow;
use core::cmp::Ordering;
use core::fmt::{self, Debug, Display, Formatter, Write};
use core::hash::{Hash, Hasher};
use core::ops::{Add, AddAssign, Deref};

use alloc::borrow::Cow;
use alloc::string::String;

use super::EcoVec;

/// Create a new [`EcoString`] from a format string.
/// ```
/// # use ecow::format_eco;
/// assert_eq!(format_eco!("Hello, {}!", 123), "Hello, 123!");
/// ```
#[macro_export]
macro_rules! format_eco {
    ($($tts:tt)*) => {{
        use ::std::fmt::Write;
        let mut s = $crate::EcoString::new();
        ::std::write!(s, $($tts)*).unwrap();
        s
    }};
}

/// An economical string with inline storage and clone-on-write semantics.
///
/// This type has a size of 16 bytes and is null-pointer optimized (meaning that
/// [`Option<EcoString>`] also takes 16 bytes). It has 14 bytes of inline
/// storage and starting from 15 bytes it becomes an [`EcoVec<u8>`]. The
/// internal reference counter of the heap variant is atomic, making this type
/// [`Sync`] and [`Send`].
///
/// # Example
/// ```
/// use ecow::EcoString;
///
/// // This is stored inline.
/// let small = EcoString::from("Welcome");
///
/// // This spills to the heap only once: `big` and `third` share the same
/// // underlying allocation. Just like vectors, heap strings are only really
/// // cloned upon mutation.
/// let big = small + " to earth! 🌱";
/// let mut third = big.clone();
/// assert_eq!(big, "Welcome to earth! 🌱");
/// assert_eq!(third, big);
///
/// // This allocates again to mutate `third` without affecting `big`.
/// assert_eq!(third.pop(), Some('🌱'));
/// assert_eq!(third, "Welcome to earth! ");
/// assert_eq!(big, "Welcome to earth! 🌱");
/// ```
#[derive(Clone)]
pub struct EcoString(Repr);

/// The internal representation. Either:
/// - inline when below a certain number of bytes, or
/// - reference-counted on the heap with clone-on-write semantics.
#[derive(Clone)]
enum Repr {
    /// Invariant: len <= LIMIT.
    Small {
        buf: [u8; LIMIT],
        len: u8,
    },
    Large(EcoVec<u8>),
}

/// The maximum number of bytes that can be stored inline.
///
/// The value is chosen such that an `EcoString` fits exactly into 16 bytes
/// (which are needed anyway due to the `Arc`s alignment, at least on 64-bit
/// platforms).
///
/// Must be at least 4 to hold any char.
pub(crate) const LIMIT: usize = 14;

impl EcoString {
    /// Create a new, empty string.
    #[inline]
    pub const fn new() -> Self {
        Self(Repr::Small { buf: [0; LIMIT], len: 0 })
    }

    /// Create a new, empty string with the given `capacity`.
    #[inline]
    pub fn with_capacity(capacity: usize) -> Self {
        if capacity <= LIMIT {
            Self::new()
        } else {
            Self(Repr::Large(EcoVec::with_capacity(capacity)))
        }
    }

    /// Create an instance from a string slice.
    #[inline]
    fn from_str(string: &str) -> Self {
        let len = string.len();
        let mut buf = [0; LIMIT];
        if let Some(head) = buf.get_mut(..len) {
            // We maintain `len < LIMIT` because `get_mut` succeeded.
            head.copy_from_slice(string.as_bytes());
            Self(Repr::Small { buf, len: len as u8 })
        } else {
            Self(Repr::Large(string.as_bytes().into()))
        }
    }

    /// Whether the string is empty.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The length of the string in bytes.
    #[inline]
    pub fn len(&self) -> usize {
        match &self.0 {
            Repr::Small { len, .. } => usize::from(*len),
            Repr::Large(string) => string.len(),
        }
    }

    /// A string slice containing the entire string.
    #[inline]
    pub fn as_str(&self) -> &str {
        let buf = match &self.0 {
            Repr::Small { buf, len } => {
                // Safety:
                // We have the invariant len <= LIMIT.
                unsafe { buf.get_unchecked(..usize::from(*len)) }
            }
            Repr::Large(vec) => vec,
        };

        // Safety:
        // The buffer contents stem from correct UTF-8 sources:
        // - Valid ASCII characters
        // - Other string slices
        // - Chars that were encoded with char::encode_utf8
        unsafe { core::str::from_utf8_unchecked(buf) }
    }

    /// Append the given character at the end.
    #[inline]
    pub fn push(&mut self, c: char) {
        if c.len_utf8() == 1 {
            match &mut self.0 {
                Repr::Small { buf, len } => {
                    let prev = usize::from(*len);
                    if let Some(slot) = buf.get_mut(prev) {
                        // We maintain `len < LIMIT` because `get_mut` succeeded.
                        *slot = c as u8;
                        *len += 1;
                        return;
                    }
                }
                Repr::Large(vec) => {
                    vec.push(c as u8);
                    return;
                }
            }
        }

        self.push_str(c.encode_utf8(&mut [0; 4]));
    }

    /// Append the given string slice at the end.
    pub fn push_str(&mut self, string: &str) {
        match &mut self.0 {
            Repr::Small { buf, len } => {
                let prev = usize::from(*len);
                let new = prev + string.len();
                if let Some(segment) = buf.get_mut(prev..new) {
                    // We maintain `len < LIMIT` because `get_mut` succeeded.
                    segment.copy_from_slice(string.as_bytes());
                    *len = new as u8;
                } else {
                    // Safety:
                    // We have the invariant `len < LIMIT`.
                    let existing = unsafe { buf.get_unchecked(..prev) };
                    let mut vec = EcoVec::with_capacity(prev + string.len());
                    vec.extend_from_byte_slice(existing);
                    vec.extend_from_byte_slice(string.as_bytes());
                    self.0 = Repr::Large(vec);
                }
            }
            Repr::Large(vec) => vec.extend_from_byte_slice(string.as_bytes()),
        }
    }

    /// Remove the last character from the string.
    #[inline]
    pub fn pop(&mut self) -> Option<char> {
        let slice = self.as_str();
        let c = slice.chars().rev().next()?;
        let shrunk = slice.len() - c.len_utf8();
        match &mut self.0 {
            Repr::Small { len, .. } => *len = shrunk as u8,
            Repr::Large(vec) => vec.truncate(shrunk),
        }
        Some(c)
    }

    /// Clear the string.
    #[inline]
    pub fn clear(&mut self) {
        match &mut self.0 {
            Repr::Small { len, .. } => *len = 0,
            Repr::Large(vec) => vec.clear(),
        }
    }

    /// Convert the string to lowercase.
    pub fn to_lowercase(&self) -> Self {
        if let Repr::Small { mut buf, len } = self.0 {
            if self.is_ascii() {
                buf[..usize::from(len)].make_ascii_lowercase();
                return Self(Repr::Small { buf, len });
            }
        }

        let str = self.as_str();
        let mut lower = Self::with_capacity(str.len());
        for c in str.chars() {
            // Let std handle the special case.
            if c == 'Σ' {
                return str.to_lowercase().into();
            }

            for v in c.to_lowercase() {
                lower.push(v);
            }
        }

        lower
    }

    /// Convert the string to uppercase.
    pub fn to_uppercase(&self) -> Self {
        if let Repr::Small { mut buf, len } = self.0 {
            if self.is_ascii() {
                buf[..usize::from(len)].make_ascii_uppercase();
                return Self(Repr::Small { buf, len });
            }
        }

        let str = self.as_str();
        let mut upper = Self::with_capacity(str.len());
        for c in str.chars() {
            for v in c.to_uppercase() {
                upper.push(v);
            }
        }

        upper
    }

    /// Repeat this string `n` times.
    pub fn repeat(&self, n: usize) -> Self {
        if n == 0 {
            return Self::new();
        }

        if let Repr::Small { buf, len } = self.0 {
            let prev = usize::from(len);
            let new = prev.saturating_mul(n);
            if new <= LIMIT {
                let src = &buf[..prev];
                let mut buf = [0; LIMIT];
                for i in 0..n {
                    buf[prev * i..prev * (i + 1)].copy_from_slice(src);
                }

                // We maintain `len < LIMIT` because of the check above.
                return Self(Repr::Small { buf, len: new as u8 });
            }
        }

        let slice = self.as_bytes();
        let capacity = slice.len().saturating_mul(n);
        let mut vec = EcoVec::with_capacity(capacity);
        for _ in 0..n {
            vec.extend_from_byte_slice(slice);
        }

        Self(Repr::Large(vec))
    }
}

impl Deref for EcoString {
    type Target = str;

    #[inline]
    fn deref(&self) -> &str {
        self.as_str()
    }
}

impl Default for EcoString {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

impl Debug for EcoString {
    #[inline]
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        Debug::fmt(self.as_str(), f)
    }
}

impl Display for EcoString {
    #[inline]
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        Display::fmt(self.as_str(), f)
    }
}

impl Eq for EcoString {}

impl PartialEq for EcoString {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        self.as_str().eq(other.as_str())
    }
}

impl PartialEq<str> for EcoString {
    #[inline]
    fn eq(&self, other: &str) -> bool {
        self.as_str().eq(other)
    }
}

impl PartialEq<&str> for EcoString {
    #[inline]
    fn eq(&self, other: &&str) -> bool {
        self.as_str().eq(*other)
    }
}

impl PartialEq<String> for EcoString {
    #[inline]
    fn eq(&self, other: &String) -> bool {
        self.as_str().eq(other)
    }
}

impl PartialEq<EcoString> for str {
    #[inline]
    fn eq(&self, other: &EcoString) -> bool {
        self.eq(other.as_str())
    }
}

impl PartialEq<EcoString> for &str {
    #[inline]
    fn eq(&self, other: &EcoString) -> bool {
        (*self).eq(other.as_str())
    }
}

impl PartialEq<EcoString> for String {
    #[inline]
    fn eq(&self, other: &EcoString) -> bool {
        self.eq(other.as_str())
    }
}

impl Ord for EcoString {
    #[inline]
    fn cmp(&self, other: &Self) -> Ordering {
        self.as_str().cmp(other.as_str())
    }
}

impl PartialOrd for EcoString {
    #[inline]
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        self.as_str().partial_cmp(other.as_str())
    }
}

impl Hash for EcoString {
    #[inline]
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.as_str().hash(state);
    }
}

impl Write for EcoString {
    #[inline]
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.push_str(s);
        Ok(())
    }

    #[inline]
    fn write_char(&mut self, c: char) -> fmt::Result {
        self.push(c);
        Ok(())
    }
}

impl Add for EcoString {
    type Output = Self;

    #[inline]
    fn add(mut self, rhs: Self) -> Self::Output {
        self += rhs;
        self
    }
}

impl AddAssign for EcoString {
    #[inline]
    fn add_assign(&mut self, rhs: Self) {
        self.push_str(rhs.as_str());
    }
}

impl Add<&str> for EcoString {
    type Output = Self;

    #[inline]
    fn add(mut self, rhs: &str) -> Self::Output {
        self += rhs;
        self
    }
}

impl AddAssign<&str> for EcoString {
    #[inline]
    fn add_assign(&mut self, rhs: &str) {
        self.push_str(rhs);
    }
}

impl AsRef<str> for EcoString {
    #[inline]
    fn as_ref(&self) -> &str {
        self
    }
}

impl Borrow<str> for EcoString {
    #[inline]
    fn borrow(&self) -> &str {
        self
    }
}

impl From<char> for EcoString {
    #[inline]
    fn from(c: char) -> Self {
        // We maintain `len < LIMIT` because `LIMIT >= 4`.
        let mut buf = [0; LIMIT];
        let len = c.encode_utf8(&mut buf).len();
        Self(Repr::Small { buf, len: len as u8 })
    }
}

impl From<&str> for EcoString {
    #[inline]
    fn from(s: &str) -> Self {
        Self::from_str(s)
    }
}

impl From<String> for EcoString {
    /// When the string does not fit inline, this needs to allocate to change
    /// the layout.
    #[inline]
    fn from(s: String) -> Self {
        Self::from_str(&s)
    }
}

impl From<Cow<'_, str>> for EcoString {
    #[inline]
    fn from(s: Cow<str>) -> Self {
        Self::from_str(&s)
    }
}

impl FromIterator<char> for EcoString {
    #[inline]
    fn from_iter<T: IntoIterator<Item = char>>(iter: T) -> Self {
        let mut s = Self::new();
        for c in iter {
            s.push(c);
        }
        s
    }
}

impl FromIterator<Self> for EcoString {
    #[inline]
    fn from_iter<T: IntoIterator<Item = Self>>(iter: T) -> Self {
        let mut s = Self::new();
        for piece in iter {
            s.push_str(&piece);
        }
        s
    }
}

impl Extend<char> for EcoString {
    #[inline]
    fn extend<T: IntoIterator<Item = char>>(&mut self, iter: T) {
        for c in iter {
            self.push(c);
        }
    }
}

impl From<EcoString> for String {
    /// This needs to allocate to change the layout.
    #[inline]
    fn from(s: EcoString) -> Self {
        s.as_str().into()
    }
}

impl From<&EcoString> for String {
    #[inline]
    fn from(s: &EcoString) -> Self {
        s.as_str().into()
    }
}
