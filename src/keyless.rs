use std::{borrow::Borrow, collections::HashMap, hash::Hash, marker::PhantomData, ops::Index};

use ph::fmph::{GOBuildConf, GOConf, GOFunction};

pub struct KeylessPerfectMap<K, V> {
    pub function: ph::fmph::GOFunction,
    pub values: Vec<V>,
    pub keys: PhantomData<K>,
}

impl<KEY: Hash + Sync, VALUE: Hash + Sync> KeylessPerfectMap<KEY, VALUE> {
    pub fn from_map_invert<U: Into<VALUE>>(map: HashMap<U, KEY>) -> KeylessPerfectMap<KEY, VALUE> {
        let (values, keys): (Vec<_>, Vec<_>) = map.into_iter().unzip();

        KeylessPerfectMap::new(keys, values)
    }
}

impl<K: Hash + Sync, V> KeylessPerfectMap<K, V> {
    pub fn from_map<U: Into<V>>(map: HashMap<K, U>) -> KeylessPerfectMap<K, V> {
        let (keys, values): (Vec<_>, Vec<_>) = map.into_iter().unzip();

        KeylessPerfectMap::new(keys, values)
    }

    pub fn new<U: Into<V>>(keys: Vec<K>, values: Vec<U>) -> KeylessPerfectMap<K, V> {
        assert!(keys.len() == values.len());

        let hasher = GOFunction::from_slice_with_conf(
            &keys,
            GOBuildConf::with_lsize(GOConf::default(), 300),
        );

        let map_len = values.len();
        let mut reordered_vals = Vec::with_capacity(map_len);

        for (k, v) in keys.into_iter().zip(values.into_iter()) {
            let new_idx = hasher.get(&k).unwrap() as usize;
            reordered_vals.spare_capacity_mut()[new_idx].write(v.into());
        }

        unsafe {
            reordered_vals.set_len(map_len);
        }

        KeylessPerfectMap {
            function: hasher,
            values: reordered_vals,
            keys: PhantomData
        }
    }

    /// gets the value associated with `key`. if `key` is not in the set, this may return a random value.
    pub fn get_unchecked<Q>(&self, key: &Q) -> Option<&V>
    where
        K: Borrow<Q>,
        Q: Hash + ?Sized,
    {
        self.function
            .get(key)
            .and_then(|v| self.values.get(v as usize))
    }

    pub fn get<Q>(&self, key: &Q) -> Option<&V>
    where
        K: Borrow<Q> + std::cmp::PartialEq,
        Q: Hash + std::cmp::PartialEq + ?Sized,
    {

        match self.function.get(key) {
            Some(idx) => {
                let idx = idx as usize;
                self.values.get(idx as usize)
            },
            None => None,
        }
    }

    pub fn values(&self) -> impl Iterator<Item = &V> {
        self.values.iter()
    }

    pub fn len(&self) -> usize {
        self.values.len()
    }
}

impl<K, Q: ?Sized, V> Index<&Q> for KeylessPerfectMap<K, V>
where
    K: Hash + Borrow<Q> + Sync + std::cmp::PartialEq,
    Q: Hash + PartialEq,
{
    type Output = V;

    /// Returns a reference to the value corresponding to the supplied key.
    ///
    /// # Panics
    ///
    /// Panics if the key is not present in the `PerfectMap`.
    #[inline]
    fn index(&self, key: &Q) -> &V {
        self.get(key).expect("no entry found for key")
    }
}

#[cfg(feature = "serde")]
impl<K: serde::Serialize, V: serde::Serialize> serde::Serialize for KeylessPerfectMap<K, V> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::{Error, SerializeStruct};

        let mut state = serializer.serialize_struct("PerfectMap", 3)?;
        state.serialize_field("values", &self.values)?;
        state.serialize_field("keys", &self.keys)?;

        let mut hasher_bytes = Vec::with_capacity(self.function.write_bytes());
        self.function
            .write(&mut hasher_bytes)
            .map_err(|_| S::Error::custom("couldn't write hash function"))?;
        state.serialize_field("function", &hasher_bytes)?;
        state.end()
    }
}

#[cfg(feature = "serde")]
impl<'de, K: serde::Deserialize<'de>, V: serde::Deserialize<'de>> serde::Deserialize<'de>
    for KeylessPerfectMap<K, V>
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(serde::Deserialize)]
        #[serde(field_identifier, rename_all = "lowercase")]
        enum Field {
            Values,
            Function,
        }

        #[repr(transparent)]
        struct PerfectMapVisitor<K, V> {
            spooky: PhantomData<(K, V)>,
        }

        impl<'de, K: serde::Deserialize<'de>, V: serde::Deserialize<'de>> serde::de::Visitor<'de>
            for PerfectMapVisitor<K, V>
        {
            type Value = KeylessPerfectMap<K, V>;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("struct PerfectMap")
            }

            fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
            where
                A: serde::de::SeqAccess<'de>,
            {
                let values: Vec<V> = seq
                    .next_element()?
                    .ok_or_else(|| serde::de::Error::invalid_length(0, &self))?;
                let function_bytes: Vec<u8> = seq
                    .next_element()?
                    .ok_or_else(|| serde::de::Error::invalid_length(1, &self))?;

                let function = GOFunction::read(&mut function_bytes.as_slice()).map_err(|_| {
                    serde::de::Error::custom(
                        "invalid bytes: expected bytes representing a ph::GOFunction",
                    )
                })?;

                Ok(KeylessPerfectMap {
                    function,
                    values,
                    keys: PhantomData
                })
            }

            fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
            where
                A: serde::de::MapAccess<'de>,
            {
                let mut values: Option<Vec<V>> = None;
                let mut function_bytes: Option<Vec<u8>> = None;

                while let Some(key) = map.next_key()? {
                    match key {
                        Field::Function => {
                            if function_bytes.is_some() {
                                return Err(serde::de::Error::duplicate_field("function"));
                            };

                            function_bytes = Some(map.next_value()?);
                        }
                        Field::Values => {
                            if values.is_some() {
                                return Err(serde::de::Error::duplicate_field("values"));
                            };
                            values = Some(map.next_value()?);
                        }
                    }
                }

                let function_bytes: Vec<u8> =
                    function_bytes.ok_or_else(|| serde::de::Error::missing_field("function"))?;
                let values = values.ok_or_else(|| serde::de::Error::missing_field("values"))?;
                let function = GOFunction::read(&mut function_bytes.as_slice()).map_err(|_| {
                    serde::de::Error::custom(
                        "invalid bytes: expected bytes representing a ph::GOFunction",
                    )
                })?;

                Ok(KeylessPerfectMap {
                    function,
                    values,
                    keys: PhantomData
                })
            }
        }

        const FIELDS: &'static [&'static str] = &["values", "function"];
        deserializer.deserialize_struct(
            "PerfectMap",
            FIELDS,
            PerfectMapVisitor {
                spooky: PhantomData,
            },
        )
    }
}