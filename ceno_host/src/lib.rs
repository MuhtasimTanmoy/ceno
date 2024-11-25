// TODO: create sp1 style host functionality.  Start with write and write_slice.

use rkyv::{Archive, Deserialize, Serialize};

#[derive(Archive, Deserialize, Serialize, Debug, PartialEq)]
#[rkyv(
    // This will generate a PartialEq impl between our unarchived
    // and archived types
    compare(PartialEq),
    // Derives can be passed through to the generated type:
    derive(Debug),
)]
struct Test {
    int: u8,
    string: String,
    option: Option<Vec<i32>>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::Rng;
    use rkyv::{
        deserialize,
        rancor::{Error, Failure},
    };

    #[test]
    fn test_rkyv_padding() {
        let value = Test {
            int: 42,
            string: "hello world".to_string(),
            option: Some(vec![1, 2, 3, 4]),
        };

        // Serializing is as easy as a single function call
        let _bytes = rkyv::to_bytes::<Error>(&value).unwrap();

        // Or you can customize your serialization for better performance or control
        // over resource usage
        use rkyv::{api::high::to_bytes_with_alloc, ser::allocator::Arena};

        let mut arena = Arena::new();
        let bytes = to_bytes_with_alloc::<_, Error>(&value, arena.acquire()).unwrap();

        // You can use the safe API for fast zero-copy deserialization
        let archived = rkyv::access::<ArchivedTest, Failure>(&bytes[..]).unwrap();
        assert_eq!(archived, &value);

        // And you can always deserialize back to the original type
        let deserialized = deserialize::<Test, Error>(archived).unwrap();
        assert_eq!(deserialized, value);

        let mut rng = rand::thread_rng();

        {
            // https://rkyv.org/format.html says:
            // This deterministic layout means that you don't need to store the position of
            // the root object in most cases. As long as your buffer ends right at the end of
            // your root object, you can use `access` with your buffer.

            // Thus left padding should work.  We add 1024 bytes of random junk to the left.

            let mut left_padded_bytes = vec![0; 1024];
            rng.fill(&mut left_padded_bytes[..]);
            // Then add our original bytes to the end:
            left_padded_bytes.extend_from_slice(&bytes);

            // we should be able to access as before:
            let archived2 = rkyv::access::<ArchivedTest, Error>(&left_padded_bytes[..]).unwrap();
            assert_eq!(archived2, &value);
        }
        {
            // The same but right padding junk should fail:
            let mut right_padded_bytes = bytes.clone();
            let mut junk = vec![0; 1024];
            rng.fill(&mut junk[..]);
            right_padded_bytes.extend_from_slice(&junk);
            // we should not be able to access as before:
            let _ = rkyv::access::<ArchivedTest, Error>(&right_padded_bytes[..])
                .expect_err("This should fail.");
        }
    }
}
