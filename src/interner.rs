// Copyright 2019-2021 Parity Technologies (UK) Ltd.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! Interning data structure and associated symbol definitions.
//!
//! The interner is used by the registry in order to deduplicate strings and type
//! definitions. Strings are uniquely identified by their contents while types
//! are uniquely identified by their respective type identifiers.
//!
//! The interners provide a strict ordered sequence of cached (interned)
//! elements and is later used for space-efficient serialization within the
//! registry.

use crate::prelude::{
    collections::btree_map::{
        BTreeMap,
        Entry,
    },
    marker::PhantomData,
    num::NonZeroU32,
    vec::Vec,
};

use crate::{
    build::Fields,
    form::MetaForm,
    Path,
    Type,
    TypeInfo,
};

#[cfg(feature = "serde")]
use serde::{
    Deserialize,
    Serialize,
};

/// A symbol that is not lifetime tracked.
///
/// This can be used by self-referential types but
/// can no longer be used to resolve instances.
#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(transparent))]
pub struct UntrackedSymbol<T> {
    /// The index to the symbol in the interner table.
    id: NonZeroU32,
    #[cfg_attr(feature = "serde", serde(skip))]
    marker: PhantomData<fn() -> T>,
}

impl<T> scale::Encode for UntrackedSymbol<T> {
    fn encode_to<W: scale::Output + ?Sized>(&self, dest: &mut W) {
        self.id.get().encode_to(dest)
    }
}

impl<T> scale::Decode for UntrackedSymbol<T> {
    fn decode<I: scale::Input>(value: &mut I) -> Result<Self, scale::Error> {
        let id = <u32 as scale::Decode>::decode(value)?;
        if id < 1 {
            return Err("UntrackedSymbol::id should be a non-zero unsigned integer".into())
        }
        let id = NonZeroU32::new(id).expect("ID is non zero");
        Ok(UntrackedSymbol {
            id,
            marker: Default::default(),
        })
    }
}

// TODO: we should be able to derive this.
impl<T> TypeInfo for UntrackedSymbol<T>
where
    T: TypeInfo + 'static,
{
    type Identity = Self;
    fn type_info() -> Type<MetaForm> {
        Type::builder()
            .path(Path::prelude("Path"))
            .composite(Fields::named().field_of::<NonZeroU32>("id", "NonZeroU32"))
    }
}

impl<T> UntrackedSymbol<T> {
    /// Returns the index to the symbol in the interner table.
    pub fn id(&self) -> NonZeroU32 {
        self.id
    }
}

/// A symbol from an interner.
///
/// Can be used to resolve to the associated instance.
#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(Serialize))]
#[cfg_attr(feature = "serde", serde(transparent))]
pub struct Symbol<'a, T> {
    id: NonZeroU32,
    #[cfg_attr(feature = "serde", serde(skip))]
    marker: PhantomData<fn() -> &'a T>,
}

impl<T> Symbol<'_, T> {
    /// Removes the lifetime tracking for this symbol.
    ///
    /// # Note
    ///
    /// - This can be useful in situations where a data structure owns all
    ///   symbols and interners and can verify accesses by itself.
    /// - For further safety reasons an untracked symbol can no longer be used
    ///   to resolve from an interner. It is still useful for serialization
    ///   purposes.
    ///
    /// # Safety
    ///
    /// Although removing lifetime constraints this operation can be
    /// considered to be safe since untracked symbols can no longer be
    /// used to resolve their associated instance from the interner.
    pub fn into_untracked(self) -> UntrackedSymbol<T> {
        UntrackedSymbol {
            id: self.id,
            marker: PhantomData,
        }
    }
}

/// Interning data structure generic over the element type.
///
/// For the sake of simplicity and correctness we are using a rather naive
/// implementation.
///
/// # Usage
///
/// This is used in order to quite efficiently cache strings and type
/// definitions uniquely identified by their associated type identifiers.
#[derive(Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize))]
#[cfg_attr(feature = "serde", serde(transparent))]
pub struct Interner<T> {
    /// A mapping from the interned elements to their respective space-efficient
    /// identifiers.
    ///
    /// The idenfitiers can be used to retrieve information about the original
    /// element from the interner.
    #[cfg_attr(feature = "serde", serde(skip))]
    map: BTreeMap<T, usize>,
    /// The ordered sequence of cached elements.
    ///
    /// This is used to efficiently provide access to the cached elements and
    /// to establish a strict ordering upon them since each is uniquely
    /// idenfitied later by its position in the vector.
    vec: Vec<T>,
}

impl<T> Interner<T>
where
    T: Ord,
{
    /// Creates a new empty interner.
    pub fn new() -> Self {
        Self {
            map: BTreeMap::new(),
            vec: Vec::new(),
        }
    }
}

impl<T: Ord> Default for Interner<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> Interner<T>
where
    T: Ord + Clone,
{
    /// Interns the given element or returns its associated symbol if it has
    /// already been interned.
    pub fn intern_or_get(&mut self, s: T) -> (bool, Symbol<T>) {
        let next_id = self.vec.len();
        let (inserted, sym_id) = match self.map.entry(s.clone()) {
            Entry::Vacant(vacant) => {
                vacant.insert(next_id);
                self.vec.push(s);
                (true, next_id)
            }
            Entry::Occupied(occupied) => (false, *occupied.get()),
        };
        (
            inserted,
            Symbol {
                id: NonZeroU32::new((sym_id + 1) as u32).unwrap(),
                marker: PhantomData,
            },
        )
    }

    /// Returns the symbol of the given element or `None` if it hasn't been
    /// interned already.
    pub fn get(&self, s: &T) -> Option<Symbol<T>> {
        self.map.get(s).map(|&id| {
            Symbol {
                id: NonZeroU32::new(id as u32).unwrap(),
                marker: PhantomData,
            }
        })
    }

    /// Resolves the original element given its associated symbol or
    /// returns `None` if it has not been interned yet.
    pub fn resolve(&self, sym: Symbol<T>) -> Option<&T> {
        let idx = (sym.id.get() - 1) as usize;
        if idx >= self.vec.len() {
            return None
        }
        self.vec.get(idx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type StringInterner = Interner<&'static str>;

    fn assert_id(
        interner: &mut StringInterner,
        new_symbol: &'static str,
        expected_id: u32,
    ) {
        let actual_id = interner.intern_or_get(new_symbol).1.id.get();
        assert_eq!(actual_id, expected_id,);
    }

    fn assert_resolve<E>(interner: &mut StringInterner, symbol_id: u32, expected_str: E)
    where
        E: Into<Option<&'static str>>,
    {
        let actual_str = interner.resolve(Symbol {
            id: NonZeroU32::new(symbol_id).unwrap(),
            marker: PhantomData,
        });
        assert_eq!(actual_str.cloned(), expected_str.into(),);
    }

    #[test]
    fn simple() {
        let mut interner = StringInterner::new();
        assert_id(&mut interner, "Hello", 1);
        assert_id(&mut interner, ", World!", 2);
        assert_id(&mut interner, "1 2 3", 3);
        assert_id(&mut interner, "Hello", 1);

        assert_resolve(&mut interner, 1, "Hello");
        assert_resolve(&mut interner, 2, ", World!");
        assert_resolve(&mut interner, 3, "1 2 3");
        assert_resolve(&mut interner, 4, None);
    }
}
