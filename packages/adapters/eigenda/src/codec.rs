pub const BYTES_PER_SYMBOL: usize = 32;

pub fn convert_by_padding_empty_byte(data: &[u8]) -> Vec<u8> {
    const PARSE_SIZE: usize = BYTES_PER_SYMBOL - 1; // 31

    let data_size = data.len();
    let data_len = data_size.div_ceil(PARSE_SIZE);

    let mut valid_data = vec![0u8; data_len * BYTES_PER_SYMBOL];
    let mut valid_end = data_len * BYTES_PER_SYMBOL;

    for i in 0..data_len {
        let start = i * PARSE_SIZE;
        let end = (i + 1) * PARSE_SIZE;
        let end = end.min(data_size);

        let output_start = i * BYTES_PER_SYMBOL;
        valid_data[output_start] = 0x00;
        let data_slice = &data[start..end];
        let output_slice = &mut valid_data[output_start + 1..output_start + 1 + data_slice.len()];
        output_slice.copy_from_slice(data_slice);

        if end == data_size {
            valid_end = output_start + 1 + (end - start);
            break;
        }
    }

    valid_data.truncate(valid_end);
    valid_data
}

#[cfg(test)]
mod tests {
    use super::*;

    fn remove_empty_byte_from_padded_bytes(data: &[u8]) -> Vec<u8> {
        const PUT_SIZE: usize = BYTES_PER_SYMBOL - 1; // 31

        let data_size = data.len();
        let data_len = data_size.div_ceil(BYTES_PER_SYMBOL);

        let mut valid_data = vec![0u8; data_len * PUT_SIZE];
        let mut valid_len = data_len * PUT_SIZE;

        for i in 0..data_len {
            let start = i * BYTES_PER_SYMBOL + 1;
            let end = (i + 1) * BYTES_PER_SYMBOL;
            let end = end.min(data_size);

            let output_start = i * PUT_SIZE;
            let data_slice = &data[start..end];
            let output_slice = &mut valid_data[output_start..output_start + data_slice.len()];
            output_slice.copy_from_slice(data_slice);

            if end == data_size {
                valid_len = output_start + data_slice.len();
                break;
            }
        }

        valid_data.truncate(valid_len);
        valid_data
    }

    #[test]
    fn test_round_trip() {
        let original = vec![0xAA; 100];
        let padded = convert_by_padding_empty_byte(&original);
        let unpadded = remove_empty_byte_from_padded_bytes(&padded);
        assert_eq!(original, unpadded);
    }

    #[test]
    fn test_edge_cases() {
        let empty: Vec<u8> = vec![];
        assert_eq!(
            empty,
            remove_empty_byte_from_padded_bytes(&convert_by_padding_empty_byte(&empty))
        );

        let small = vec![0xBB];
        assert_eq!(
            small,
            remove_empty_byte_from_padded_bytes(&convert_by_padding_empty_byte(&small))
        );

        // exact multiple of 31
        let exact = vec![0xCC; 31 * 4];
        assert_eq!(
            exact,
            remove_empty_byte_from_padded_bytes(&convert_by_padding_empty_byte(&exact))
        );
    }

    #[test]
    fn test_padding() {
        // test padding of 31 bytes becomes 32 bytes
        let input31 = vec![1; 31];
        let padded = convert_by_padding_empty_byte(&input31);
        assert_eq!(padded.len(), 32);
        assert_eq!(padded[0], 0x00);
        assert_eq!(&padded[1..], &input31[..]);

        // test padding of 32 bytes becomes 34 bytes
        let input32 = vec![2; 32];
        let padded = convert_by_padding_empty_byte(&input32);
        assert_eq!(padded.len(), 34);
        assert_eq!(padded[0], 0x00);
        assert_eq!(&padded[1..32], &input32[0..31]);
        assert_eq!(padded[32], 0x00);
        assert_eq!(padded[33], 2);
    }
}
