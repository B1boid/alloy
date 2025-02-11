use crate::{Id, Response, SerializedRequest};
use serde::{
    de::{self, Deserializer, MapAccess, SeqAccess, Visitor},
    Deserialize, Serialize,
};
use serde_json::value::RawValue;
use std::{collections::HashSet, fmt, marker::PhantomData};

/// A [`RequestPacket`] is a [`SerializedRequest`] or a batch of serialized
/// request.
#[derive(Debug, Clone)]
pub enum RequestPacket {
    /// A single request.
    Single(SerializedRequest),
    /// A batch of requests.
    Batch(Vec<SerializedRequest>),
}

impl FromIterator<SerializedRequest> for RequestPacket {
    fn from_iter<T: IntoIterator<Item = SerializedRequest>>(iter: T) -> Self {
        Self::Batch(iter.into_iter().collect())
    }
}

impl From<SerializedRequest> for RequestPacket {
    fn from(req: SerializedRequest) -> Self {
        Self::Single(req)
    }
}

impl Serialize for RequestPacket {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            RequestPacket::Single(single) => single.serialize(serializer),
            RequestPacket::Batch(batch) => batch.serialize(serializer),
        }
    }
}

impl RequestPacket {
    /// Create a new empty packet with the given capacity.
    pub fn with_capacity(capacity: usize) -> Self {
        Self::Batch(Vec::with_capacity(capacity))
    }

    /// Serialize the packet as a boxed [`RawValue`].
    pub fn serialize(self) -> serde_json::Result<Box<RawValue>> {
        match self {
            RequestPacket::Single(single) => Ok(single.take_request()),
            RequestPacket::Batch(batch) => serde_json::value::to_raw_value(&batch),
        }
    }

    /// Get the request IDs of all subscription requests in the packet.
    pub fn subscription_request_ids(&self) -> HashSet<&Id> {
        match self {
            RequestPacket::Single(single) => {
                let mut hs = HashSet::with_capacity(1);
                if single.method() == "eth_subscribe" {
                    hs.insert(single.id());
                }
                hs
            }
            RequestPacket::Batch(batch) => batch
                .iter()
                .filter(|req| req.method() == "eth_subscribe")
                .map(|req| req.id())
                .collect(),
        }
    }

    /// Get the number of requests in the packet.
    pub fn len(&self) -> usize {
        match self {
            Self::Single(_) => 1,
            Self::Batch(batch) => batch.len(),
        }
    }

    /// Check if the packet is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Push a request into the packet.
    pub fn push(&mut self, req: SerializedRequest) {
        if let Self::Batch(batch) = self {
            batch.push(req);
            return;
        }
        if matches!(self, Self::Single(_)) {
            let old = std::mem::replace(self, Self::Batch(Vec::with_capacity(10)));
            match old {
                Self::Single(single) => {
                    self.push(single);
                }
                _ => unreachable!(),
            }
            self.push(req);
        }
    }
}

/// A [`ResponsePacket`] is a [`Response`] or a batch of responses.
#[derive(Debug, Clone)]
pub enum ResponsePacket<Payload = Box<RawValue>, ErrData = Box<RawValue>> {
    /// A single response.
    Single(Response<Payload, ErrData>),
    /// A batch of responses.
    Batch(Vec<Response<Payload, ErrData>>),
}

impl<Payload, ErrData> FromIterator<Response<Payload, ErrData>>
    for ResponsePacket<Payload, ErrData>
{
    fn from_iter<T: IntoIterator<Item = Response<Payload, ErrData>>>(iter: T) -> Self {
        let mut iter = iter.into_iter().peekable();
        // return single if iter has exactly one element, else make a batch
        if let Some(first) = iter.next() {
            if iter.peek().is_none() {
                return Self::Single(first);
            } else {
                let mut batch = Vec::new();
                batch.push(first);
                batch.extend(iter);
                return Self::Batch(batch);
            }
        }
        Self::Batch(vec![])
    }
}

impl<Payload, ErrData> From<Vec<Response<Payload, ErrData>>> for ResponsePacket<Payload, ErrData> {
    fn from(value: Vec<Response<Payload, ErrData>>) -> Self {
        if value.len() == 1 {
            Self::Single(value.into_iter().next().unwrap())
        } else {
            Self::Batch(value)
        }
    }
}

impl<'de, Payload, ErrData> Deserialize<'de> for ResponsePacket<Payload, ErrData>
where
    Payload: Deserialize<'de>,
    ErrData: Deserialize<'de>,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct ResponsePacketVisitor<Payload, ErrData> {
            marker: PhantomData<fn() -> ResponsePacket<Payload, ErrData>>,
        }

        impl<'de, Payload, ErrData> Visitor<'de> for ResponsePacketVisitor<Payload, ErrData>
        where
            Payload: Deserialize<'de>,
            ErrData: Deserialize<'de>,
        {
            type Value = ResponsePacket<Payload, ErrData>;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a single response or a batch of responses")
            }

            fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
            where
                A: SeqAccess<'de>,
            {
                let mut responses = Vec::new();

                while let Some(response) = seq.next_element()? {
                    responses.push(response);
                }

                Ok(ResponsePacket::Batch(responses))
            }

            fn visit_map<M>(self, map: M) -> Result<Self::Value, M::Error>
            where
                M: MapAccess<'de>,
            {
                let response =
                    Deserialize::deserialize(de::value::MapAccessDeserializer::new(map))?;
                Ok(ResponsePacket::Single(response))
            }
        }

        deserializer.deserialize_any(ResponsePacketVisitor { marker: PhantomData })
    }
}

/// A [`BorrowedResponsePacket`] is a [`ResponsePacket`] that has been partially
/// deserialized, borrowing its contents from the deserializer. This is used
/// primarily for intermediate deserialization. Most users will not require it.
///
/// See the [top-level docs] for more info.
///
/// [top-level docs]: crate
pub type BorrowedResponsePacket<'a> = ResponsePacket<&'a RawValue, &'a RawValue>;

impl BorrowedResponsePacket<'_> {
    /// Convert this borrowed response packet into an owned packet by copying
    /// the data from the deserializer (if necessary).
    pub fn into_owned(self) -> ResponsePacket {
        match self {
            Self::Single(single) => ResponsePacket::Single(single.into_owned()),
            Self::Batch(batch) => {
                ResponsePacket::Batch(batch.into_iter().map(Response::into_owned).collect())
            }
        }
    }
}

impl<Payload, ErrData> ResponsePacket<Payload, ErrData> {
    /// Find responses by a list of IDs.
    ///
    /// This is intended to be used in conjunction with
    /// [`RequestPacket::subscription_request_ids`] to identify subscription
    /// responses.
    ///
    /// # Note
    ///
    /// - Responses are not guaranteed to be in the same order.
    /// - Responses are not guaranteed to be in the set.
    /// - If the packet contains duplicate IDs, both will be found.
    pub fn responses_by_ids(&self, ids: &HashSet<Id>) -> Vec<&Response<Payload, ErrData>> {
        match self {
            Self::Single(single) => {
                let mut resps = Vec::new();
                if ids.contains(&single.id) {
                    resps.push(single);
                }
                resps
            }
            Self::Batch(batch) => batch.iter().filter(|res| ids.contains(&res.id)).collect(),
        }
    }
}
