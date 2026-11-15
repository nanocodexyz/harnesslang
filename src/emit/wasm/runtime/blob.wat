(global $TYPE_BLOB i32 (i32.const 7))
(global $VARIANT_BLOB i32 (i32.const 3))
(global $SHA256_BLOCK_BYTES i32 (i32.const 64))
(global $SHA256_LENGTH_BYTES i32 (i32.const 8))
(global $SHA256_ROUNDS i32 (i32.const 64))
(global $SHA256_SCHEDULE_FROM_BLOCK i32 (i32.const 16))
(global $SHA256_STATE_BYTES i32 (i32.const 32))
(global $SHA256_STATE_WORDS i32 (i32.const 8))
(global $SHA256_PAD_BYTE i32 (i32.const 0x80))
(global $BASE64_GROUP_BYTES i32 (i32.const 3))
(global $BASE64_GROUP_CHARS i32 (i32.const 4))
(global $BASE64_PAD i32 (i32.const 61))
(global $BASE64_PADDING_MAX i32 (i32.const 2))
(global $BASE64_SEXTET_BITS i32 (i32.const 6))
(global $DIGEST_HEX_LEN i32 (i32.const 64))
(global $NIBBLE_BITS i32 (i32.const 4))
(global $NIBBLE_MASK i32 (i32.const 15))
(global $BYTE_MASK i32 (i32.const 0xff))
(global $ASCII_PRINTABLE_FIRST i32 (i32.const 0x20))
(global $ASCII_PRINTABLE_LAST i32 (i32.const 0x7e))
(global $LATIN1_PRINTABLE_FIRST i32 (i32.const 0xa1))
(global $LATIN1_SOFT_HYPHEN i32 (i32.const 0xad))
(global $UTF8_TWO_LEAD i32 (i32.const 0xc0))
(global $UTF8_CONTINUATION_BITS i32 (i32.const 6))
(global $UTF8_CONTINUATION_MASK i32 (i32.const 0x3f))
(global $ASCII_BACKSLASH i32 (i32.const 0x5c))
(global $ASCII_APOSTROPHE i32 (i32.const 0x27))
(global $ASCII_NUL i32 (i32.const 0))
(global $ASCII_LOWER_A i32 (i32.const 0x61))
(global $ASCII_LOWER_F i32 (i32.const 0x66))
(global $BLOB_ENTRY_BYTES i32 (i32.const 8))
(global $blob_held (mut i32) (i32.const 0))
(global $base64_error (mut i32) (i32.const 0))
(global $blob_error (mut i32) (i32.const 0))
(words $sha256_initial 0x6a09e667 0xbb67ae85 0x3c6ef372 0xa54ff53a 0x510e527f 0x9b05688c 0x1f83d9ab 0x5be0cd19)
(words $sha256_constants 0x428a2f98 0x71374491 0xb5c0fbcf 0xe9b5dba5 0x3956c25b 0x59f111f1 0x923f82a4 0xab1c5ed5 0xd807aa98 0x12835b01 0x243185be 0x550c7dc3 0x72be5d74 0x80deb1fe 0x9bdc06a7 0xc19bf174 0xe49b69c1 0xefbe4786 0x0fc19dc6 0x240ca1cc 0x2de92c6f 0x4a7484aa 0x5cb0a9dc 0x76f988da 0x983e5152 0xa831c66d 0xb00327c8 0xbf597fc7 0xc6e00bf3 0xd5a79147 0x06ca6351 0x14292967 0x27b70a85 0x2e1b2138 0x4d2c6dfc 0x53380d13 0x650a7354 0x766a0abb 0x81c2c92e 0x92722c85 0xa2bfe8a1 0xa81a664b 0xc24b8b70 0xc76c51a3 0xd192e819 0xd6990624 0xf40e3585 0x106aa070 0x19a4c116 0x1e376c08 0x2748774c 0x34b0bcb5 0x391c0cb3 0x4ed8aa4a 0x5b9cca4f 0x682e6ff3 0x748f82ee 0x78a5636f 0x84c87814 0x8cc70208 0x90befffa 0xa4506ceb 0xbef9a3f7 0xc67178f2)
(text $s_base64_alphabet "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/")
(text $s_blob "blob")
(text $s_data "data")
(text $s_m_blobs "{\"blobs\":")
(global $ASCII_COMMA i32 (i32.const 44))
(text $s_b64_characters " characters, not a multiple of ")
(text $s_b64_padding "padding inside the text")
(text $s_b64_character "character ")
(text $s_b64_nonzero "nonzero bits after the last byte")
(text $s_escape_n "\\n")
(text $s_escape_r "\\r")
(text $s_escape_t "\\t")
(text $s_escape_0 "\\0")
(text $s_escape_u_open "\\u{")
(text $s_close_brace "}")
(text $s_blob_expected "expected a blob, found ")
(text $s_blob_expected_digest "expected a blob's sha-256 digest, found ")
(text $s_blob_holds "a blob holds its digest and data, found ")
(text $s_blob_returned " returned ")
(text $s_blob_not_blob ", which is not a blob: a host returns a blob as {\"data\": base64}")
(text $s_blob_not_base64 " returned text that is not base64 (")
(text $s_blob_not_base64_close "), which is not a blob")
(text $s_blob_of " returned a blob of ")
(text $s_blob_over " bytes, over its bound ")
(text $s_space_paren " (")
(text $s_blob_open "blob ")
(text $s_blob_has_data " has data ")
(text $s_blob_not_text ", not text")
(text $s_blob_data_not_base64 " has data that is not base64: ")
(text $s_blob_digest_is " holds data whose digest is ")
(text $s_blob_without " is journaled without its data, and no earlier record holds it")
(text $s_blob_result "result ")
(text $s_blob_state "the state holds blob ")
(text $s_blob_state_close ", which no result of the journal holds")
(text $s_blob_omitted "[image omitted from this request, which keeps only the newest ")
(text $s_blob_omitted_middle " images: sha256:")
(text $s_blob_omitted_close "]")
(text $s_blob_checked_base64 "a checked blob is base64")
(text $s_blob_keeps "keep blobs leaves at most its count of blobs")
(text $s_blob_sha_check "SHA-256 does not match its catalogued check value")
(text $s_blob_sha_check_input "abc")
(text $s_blob_sha_check_hex "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
(func $be32 (param $at i32) (result i32)
  (i32.or
    (i32.or (i32.shl (i32.load8_u (local.get $at)) (i32.const 24)) (i32.shl (i32.load8_u offset=1 (local.get $at)) (i32.const 16)))
    (i32.or (i32.shl (i32.load8_u offset=2 (local.get $at)) (i32.const 8)) (i32.load8_u offset=3 (local.get $at)))))
(func $sha256_word (param $words i32) (param $index i32) (result i32)
  (i32.load (i32.add (local.get $words) (i32.shl (local.get $index) (i32.const 2)))))
(func $sha256_schedule (param $block i32) (param $w i32) (local $index i32) (local $early i32) (local $late i32)
  block $loaded
    loop $load
      (br_if $loaded (i32.ge_u (local.get $index) (global.get $SHA256_SCHEDULE_FROM_BLOCK)))
      (i32.store (i32.add (local.get $w) (i32.shl (local.get $index) (i32.const 2))) (call $be32 (i32.add (local.get $block) (i32.shl (local.get $index) (i32.const 2)))))
      (local.set $index (i32.add (local.get $index) (i32.const 1)))
      br $load
    end
  end
  block $extended
    loop $extend
      (br_if $extended (i32.ge_u (local.get $index) (global.get $SHA256_ROUNDS)))
      (local.set $early (call $sha256_word (local.get $w) (i32.sub (local.get $index) (i32.const 15))))
      (local.set $late (call $sha256_word (local.get $w) (i32.sub (local.get $index) (i32.const 2))))
      (i32.store (i32.add (local.get $w) (i32.shl (local.get $index) (i32.const 2)))
        (i32.add
          (i32.add (call $sha256_word (local.get $w) (i32.sub (local.get $index) (i32.const 16)))
            (i32.xor (i32.xor (i32.rotr (local.get $early) (i32.const 7)) (i32.rotr (local.get $early) (i32.const 18))) (i32.shr_u (local.get $early) (i32.const 3))))
          (i32.add (call $sha256_word (local.get $w) (i32.sub (local.get $index) (i32.const 7)))
            (i32.xor (i32.xor (i32.rotr (local.get $late) (i32.const 17)) (i32.rotr (local.get $late) (i32.const 19))) (i32.shr_u (local.get $late) (i32.const 10))))))
      (local.set $index (i32.add (local.get $index) (i32.const 1)))
      br $extend
    end
  end)
(func $sha256_block (param $hash i32) (param $block i32) (param $w i32) (local $index i32) (local $a i32) (local $b i32) (local $c i32) (local $d i32) (local $e i32) (local $f i32) (local $g i32) (local $h i32) (local $first i32) (local $second i32)
  (call $sha256_schedule (local.get $block) (local.get $w))
  (local.set $a (i32.load (local.get $hash)))
  (local.set $b (i32.load offset=4 (local.get $hash)))
  (local.set $c (i32.load offset=8 (local.get $hash)))
  (local.set $d (i32.load offset=12 (local.get $hash)))
  (local.set $e (i32.load offset=16 (local.get $hash)))
  (local.set $f (i32.load offset=20 (local.get $hash)))
  (local.set $g (i32.load offset=24 (local.get $hash)))
  (local.set $h (i32.load offset=28 (local.get $hash)))
  block $done
    loop $round
      (br_if $done (i32.ge_u (local.get $index) (global.get $SHA256_ROUNDS)))
      (local.set $first
        (i32.add
          (i32.add (local.get $h) (i32.xor (i32.xor (i32.rotr (local.get $e) (i32.const 6)) (i32.rotr (local.get $e) (i32.const 11))) (i32.rotr (local.get $e) (i32.const 25))))
          (i32.add
            (i32.xor (i32.and (local.get $e) (local.get $f)) (i32.and (i32.xor (local.get $e) (i32.const -1)) (local.get $g)))
            (i32.add (call $sha256_word (i32.const $sha256_constants) (local.get $index)) (call $sha256_word (local.get $w) (local.get $index))))))
      (local.set $second
        (i32.add
          (i32.xor (i32.xor (i32.rotr (local.get $a) (i32.const 2)) (i32.rotr (local.get $a) (i32.const 13))) (i32.rotr (local.get $a) (i32.const 22)))
          (i32.xor (i32.xor (i32.and (local.get $a) (local.get $b)) (i32.and (local.get $a) (local.get $c))) (i32.and (local.get $b) (local.get $c)))))
      (local.set $h (local.get $g))
      (local.set $g (local.get $f))
      (local.set $f (local.get $e))
      (local.set $e (i32.add (local.get $d) (local.get $first)))
      (local.set $d (local.get $c))
      (local.set $c (local.get $b))
      (local.set $b (local.get $a))
      (local.set $a (i32.add (local.get $first) (local.get $second)))
      (local.set $index (i32.add (local.get $index) (i32.const 1)))
      br $round
    end
  end
  (i32.store (local.get $hash) (i32.add (i32.load (local.get $hash)) (local.get $a)))
  (i32.store offset=4 (local.get $hash) (i32.add (i32.load offset=4 (local.get $hash)) (local.get $b)))
  (i32.store offset=8 (local.get $hash) (i32.add (i32.load offset=8 (local.get $hash)) (local.get $c)))
  (i32.store offset=12 (local.get $hash) (i32.add (i32.load offset=12 (local.get $hash)) (local.get $d)))
  (i32.store offset=16 (local.get $hash) (i32.add (i32.load offset=16 (local.get $hash)) (local.get $e)))
  (i32.store offset=20 (local.get $hash) (i32.add (i32.load offset=20 (local.get $hash)) (local.get $f)))
  (i32.store offset=24 (local.get $hash) (i32.add (i32.load offset=24 (local.get $hash)) (local.get $g)))
  (i32.store offset=28 (local.get $hash) (i32.add (i32.load offset=28 (local.get $hash)) (local.get $h))))
(func $buf_hex_byte (param $buffer i32) (param $byte i32)
  (call $buf_byte (local.get $buffer) (call $text_byte (i32.const $s_hex) (i32.shr_u (local.get $byte) (global.get $NIBBLE_BITS))))
  (call $buf_byte (local.get $buffer) (call $text_byte (i32.const $s_hex) (i32.and (local.get $byte) (global.get $NIBBLE_MASK)))))
(func $sha256_hex (param $bytes i32) (param $length i32) (result i32) (local $hash i32) (local $w i32) (local $at i32) (local $full i32) (local $rest i32) (local $tail i32) (local $tail_length i32) (local $bits i64) (local $index i32) (local $buffer i32) (local $word i32)
  (local.set $hash (call $alloc (global.get $SHA256_STATE_BYTES)))
  (memory.copy (local.get $hash) (i32.const $sha256_initial) (global.get $SHA256_STATE_BYTES))
  (local.set $w (call $alloc (i32.shl (global.get $SHA256_ROUNDS) (i32.const 2))))
  (local.set $full (i32.sub (local.get $length) (i32.rem_u (local.get $length) (global.get $SHA256_BLOCK_BYTES))))
  block $blocks_done
    loop $blocks
      (br_if $blocks_done (i32.ge_u (local.get $at) (local.get $full)))
      (call $sha256_block (local.get $hash) (i32.add (local.get $bytes) (local.get $at)) (local.get $w))
      (local.set $at (i32.add (local.get $at) (global.get $SHA256_BLOCK_BYTES)))
      br $blocks
    end
  end
  (local.set $rest (i32.sub (local.get $length) (local.get $full)))
  (local.set $tail (call $alloc (i32.shl (global.get $SHA256_BLOCK_BYTES) (i32.const 1))))
  (memory.fill (local.get $tail) (i32.const 0) (i32.shl (global.get $SHA256_BLOCK_BYTES) (i32.const 1)))
  (memory.copy (local.get $tail) (i32.add (local.get $bytes) (local.get $full)) (local.get $rest))
  (i32.store8 (i32.add (local.get $tail) (local.get $rest)) (global.get $SHA256_PAD_BYTE))
  (local.set $tail_length (global.get $SHA256_BLOCK_BYTES))
  (if (i32.gt_u (i32.add (i32.add (local.get $rest) (i32.const 1)) (global.get $SHA256_LENGTH_BYTES)) (global.get $SHA256_BLOCK_BYTES))
    (then (local.set $tail_length (i32.shl (global.get $SHA256_BLOCK_BYTES) (i32.const 1)))))
  (local.set $bits (i64.shl (i64.extend_i32_u (local.get $length)) (i64.const 3)))
  (local.set $index (i32.const 1))
  block $length_done
    loop $length_bytes
      (br_if $length_done (i32.gt_u (local.get $index) (global.get $SHA256_LENGTH_BYTES)))
      (i64.store8 (i32.sub (i32.add (local.get $tail) (local.get $tail_length)) (local.get $index)) (local.get $bits))
      (local.set $bits (i64.shr_u (local.get $bits) (i64.const 8)))
      (local.set $index (i32.add (local.get $index) (i32.const 1)))
      br $length_bytes
    end
  end
  (call $sha256_block (local.get $hash) (local.get $tail) (local.get $w))
  (if (i32.gt_u (local.get $tail_length) (global.get $SHA256_BLOCK_BYTES))
    (then (call $sha256_block (local.get $hash) (i32.add (local.get $tail) (global.get $SHA256_BLOCK_BYTES)) (local.get $w))))
  (local.set $buffer (call $buf_new))
  (local.set $index (i32.const 0))
  block $hex_done
    loop $hex
      (br_if $hex_done (i32.ge_u (local.get $index) (global.get $SHA256_STATE_WORDS)))
      (local.set $word (call $sha256_word (local.get $hash) (local.get $index)))
      (call $buf_hex_byte (local.get $buffer) (i32.shr_u (local.get $word) (i32.const 24)))
      (call $buf_hex_byte (local.get $buffer) (i32.and (i32.shr_u (local.get $word) (i32.const 16)) (global.get $BYTE_MASK)))
      (call $buf_hex_byte (local.get $buffer) (i32.and (i32.shr_u (local.get $word) (i32.const 8)) (global.get $BYTE_MASK)))
      (call $buf_hex_byte (local.get $buffer) (i32.and (local.get $word) (global.get $BYTE_MASK)))
      (local.set $index (i32.add (local.get $index) (i32.const 1)))
      br $hex
    end
  end
  (call $buf_finish (local.get $buffer)))
(func $sha256_check
  (call $assert
    (call $text_eq (call $sha256_hex (call $text_bytes (i32.const $s_blob_sha_check_input)) (call $text_len (i32.const $s_blob_sha_check_input))) (i32.const $s_blob_sha_check_hex))
    (i32.const $s_blob_sha_check)))
(func $base64_sextet (param $byte i32) (result i32)
  (i32.sub (i32.load8_u offset=4 (i32.add (i32.const $base64_table) (local.get $byte))) (i32.const 1)))
(func $char_debug (param $byte i32) (result i32) (local $buffer i32) (local $digits i32)
  (local.set $buffer (call $buf_new))
  (call $buf_byte (local.get $buffer) (global.get $ASCII_APOSTROPHE))
  (block $written
    (if (i32.or (i32.eq (local.get $byte) (global.get $ASCII_BACKSLASH)) (i32.eq (local.get $byte) (global.get $ASCII_APOSTROPHE)))
      (then
        (call $buf_byte (local.get $buffer) (global.get $ASCII_BACKSLASH))
        (call $buf_byte (local.get $buffer) (local.get $byte))
        (br $written)))
    (if (i32.eq (local.get $byte) (global.get $ASCII_NEWLINE)) (then (call $buf_text (local.get $buffer) (i32.const $s_escape_n)) (br $written)))
    (if (i32.eq (local.get $byte) (global.get $ASCII_RETURN)) (then (call $buf_text (local.get $buffer) (i32.const $s_escape_r)) (br $written)))
    (if (i32.eq (local.get $byte) (global.get $ASCII_TAB)) (then (call $buf_text (local.get $buffer) (i32.const $s_escape_t)) (br $written)))
    (if (i32.eq (local.get $byte) (global.get $ASCII_NUL)) (then (call $buf_text (local.get $buffer) (i32.const $s_escape_0)) (br $written)))
    (if (i32.and (i32.ge_u (local.get $byte) (global.get $ASCII_PRINTABLE_FIRST)) (i32.le_u (local.get $byte) (global.get $ASCII_PRINTABLE_LAST)))
      (then (call $buf_byte (local.get $buffer) (local.get $byte)) (br $written)))
    (if (i32.and (i32.ge_u (local.get $byte) (global.get $LATIN1_PRINTABLE_FIRST)) (i32.ne (local.get $byte) (global.get $LATIN1_SOFT_HYPHEN)))
      (then
        (call $buf_byte (local.get $buffer) (i32.or (global.get $UTF8_TWO_LEAD) (i32.shr_u (local.get $byte) (global.get $UTF8_CONTINUATION_BITS))))
        (call $buf_byte (local.get $buffer) (i32.or (global.get $UTF8_CONTINUATION_FIRST) (i32.and (local.get $byte) (global.get $UTF8_CONTINUATION_MASK))))
        (br $written)))
    (call $buf_text (local.get $buffer) (i32.const $s_escape_u_open))
    (if (i32.gt_u (local.get $byte) (global.get $NIBBLE_MASK))
      (then (call $buf_byte (local.get $buffer) (call $text_byte (i32.const $s_hex) (i32.shr_u (local.get $byte) (global.get $NIBBLE_BITS))))))
    (call $buf_byte (local.get $buffer) (call $text_byte (i32.const $s_hex) (i32.and (local.get $byte) (global.get $NIBBLE_MASK))))
    (call $buf_text (local.get $buffer) (i32.const $s_close_brace)))
  (call $buf_byte (local.get $buffer) (global.get $ASCII_APOSTROPHE))
  (call $buf_finish (local.get $buffer)))
(func $base64_fail (param $error i32) (result i32)
  (global.set $base64_error (local.get $error))
  (i32.const 0))
(func $base64_decode (param $text i32) (result i32) (local $length i32) (local $groups i32) (local $out i32) (local $group i32) (local $base i32) (local $padding i32) (local $joined i32) (local $at i32) (local $sextet i32) (local $kept i32) (local $written i32) (local $buffer i32)
  (global.set $base64_error (i32.const 0))
  (local.set $length (call $text_len (local.get $text)))
  (if (i32.rem_u (local.get $length) (global.get $BASE64_GROUP_CHARS))
    (then
      (local.set $buffer (call $buf_new))
      (call $buf_u64 (local.get $buffer) (i64.extend_i32_u (local.get $length)))
      (call $buf_text (local.get $buffer) (i32.const $s_b64_characters))
      (call $buf_u64 (local.get $buffer) (i64.extend_i32_u (global.get $BASE64_GROUP_CHARS)))
      (return (call $base64_fail (call $buf_finish (local.get $buffer))))))
  (local.set $groups (i32.div_u (local.get $length) (global.get $BASE64_GROUP_CHARS)))
  (local.set $out (call $text_new (i32.mul (local.get $groups) (global.get $BASE64_GROUP_BYTES))))
  block $decoded
    loop $each_group
      (br_if $decoded (i32.ge_u (local.get $group) (local.get $groups)))
      (local.set $base (i32.add (call $text_bytes (local.get $text)) (i32.mul (local.get $group) (global.get $BASE64_GROUP_CHARS))))
      (local.set $padding (i32.const 0))
      block $counted
        loop $count
          (br_if $counted (i32.ge_u (local.get $padding) (global.get $BASE64_GROUP_CHARS)))
          (br_if $counted (i32.ne (i32.load8_u (i32.sub (i32.add (local.get $base) (i32.sub (global.get $BASE64_GROUP_CHARS) (i32.const 1))) (local.get $padding))) (global.get $BASE64_PAD)))
          (local.set $padding (i32.add (local.get $padding) (i32.const 1)))
          br $count
        end
      end
      (if (i32.or (i32.gt_u (local.get $padding) (global.get $BASE64_PADDING_MAX))
            (i32.and (i32.ne (local.get $padding) (i32.const 0)) (i32.ne (i32.add (local.get $group) (i32.const 1)) (local.get $groups))))
        (then (return (call $base64_fail (i32.const $s_b64_padding)))))
      (local.set $joined (i32.const 0))
      (local.set $at (i32.const 0))
      block $joined_all
        loop $join
          (br_if $joined_all (i32.ge_u (local.get $at) (i32.sub (global.get $BASE64_GROUP_CHARS) (local.get $padding))))
          (local.set $sextet (call $base64_sextet (i32.load8_u (i32.add (local.get $base) (local.get $at)))))
          (if (i32.lt_s (local.get $sextet) (i32.const 0))
            (then (return (call $base64_fail (call $text_concat (i32.const $s_b64_character) (call $char_debug (i32.load8_u (i32.add (local.get $base) (local.get $at)))))))))
          (local.set $joined (i32.or (i32.shl (local.get $joined) (global.get $BASE64_SEXTET_BITS)) (local.get $sextet)))
          (local.set $at (i32.add (local.get $at) (i32.const 1)))
          br $join
        end
      end
      (local.set $joined (i32.shl (local.get $joined) (i32.mul (global.get $BASE64_SEXTET_BITS) (local.get $padding))))
      (local.set $kept (i32.sub (global.get $BASE64_GROUP_BYTES) (local.get $padding)))
      (if (i32.and (local.get $joined) (i32.sub (i32.shl (i32.const 1) (i32.mul (local.get $padding) (global.get $BITS_PER_BYTE))) (i32.const 1)))
        (then (return (call $base64_fail (i32.const $s_b64_nonzero)))))
      (local.set $at (i32.const 0))
      block $stored
        loop $store
          (br_if $stored (i32.ge_u (local.get $at) (local.get $kept)))
          (i32.store8 (i32.add (call $text_bytes (local.get $out)) (local.get $written))
            (i32.shr_u (local.get $joined) (i32.mul (global.get $BITS_PER_BYTE) (i32.sub (i32.sub (global.get $BASE64_GROUP_BYTES) (i32.const 1)) (local.get $at)))))
          (local.set $written (i32.add (local.get $written) (i32.const 1)))
          (local.set $at (i32.add (local.get $at) (i32.const 1)))
          br $store
        end
      end
      (local.set $group (i32.add (local.get $group) (i32.const 1)))
      br $each_group
    end
  end
  (i32.store (local.get $out) (local.get $written))
  (local.get $out))
(func $blobs_has (result i32) (i32.load (i32.const $blobs)))
(func $blobs_dispatched (result i32) (i32.load offset=4 (i32.const $blobs)))
(func $blobs_attached (result i32) (i32.load offset=8 (i32.const $blobs)))
(func $blobs_result_variant (result i32) (i32.load offset=12 (i32.const $blobs)))
(func $dispatched_sum (param $type i32) (result i32)
  (if (i32.eqz (call $blobs_has)) (then (return (i32.const 0))))
  (if (i32.ne (call $type_kind (local.get $type)) (global.get $TYPE_SUM)) (then (return (i32.const 0))))
  (i32.eq (i32.load offset=4 (local.get $type)) (call $blobs_dispatched)))
(func $reports_unknown (param $type i32) (result i32)
  (i32.or (i32.eq (call $type_kind (local.get $type)) (global.get $TYPE_MAYBE)) (call $dispatched_sum (local.get $type))))
(func $tool_more (param $name i32) (result i32) (local $index i32) (local $tool i32)
  block $absent
    loop $next
      (br_if $absent (i32.ge_u (local.get $index) (i32.load offset=28 (i32.const $program))))
      (local.set $tool (i32.load (i32.add (i32.load offset=32 (i32.const $program)) (i32.shl (local.get $index) (i32.const 2)))))
      (if (call $text_eq (i32.load (local.get $tool)) (local.get $name))
        (then (return (i32.load (i32.add (i32.const $tools_more) (i32.shl (local.get $index) (i32.const 2)))))))
      (local.set $index (i32.add (local.get $index) (i32.const 1)))
      br $next
    end
  end
  (i32.const 0))
(func $tool_blob (param $name i32) (result i32) (local $more i32)
  (local.set $more (call $tool_more (local.get $name)))
  (if (i32.eqz (local.get $more)) (then (return (i32.const 0))))
  (if (i32.eqz (i32.load (local.get $more))) (then (return (i32.const 0))))
  (local.get $more))
(func $tool_finds (param $name i32) (result i32) (local $more i32)
  (local.set $more (call $tool_more (local.get $name)))
  (if (i32.eqz (local.get $more)) (then (return (i32.const 0))))
  (i32.load offset=12 (local.get $more)))
(func $blob_value_json (param $value i64) (result i32)
  (call $json_one (i32.const $s_blob) (call $json_string (call $value_text (local.get $value)))))
(func $is_digest (param $text i32) (result i32) (local $at i32) (local $byte i32)
  (if (i32.ne (call $text_len (local.get $text)) (global.get $DIGEST_HEX_LEN)) (then (return (i32.const 0))))
  block $done
    loop $next
      (br_if $done (i32.ge_u (local.get $at) (global.get $DIGEST_HEX_LEN)))
      (local.set $byte (call $text_byte (local.get $text) (local.get $at)))
      (if (i32.eqz (i32.or (call $is_digit (local.get $byte))
            (i32.and (i32.ge_u (local.get $byte) (global.get $ASCII_LOWER_A)) (i32.le_u (local.get $byte) (global.get $ASCII_LOWER_F)))))
        (then (return (i32.const 0))))
      (local.set $at (i32.add (local.get $at) (i32.const 1)))
      br $next
    end
  end
  (i32.const 1))
(func $blob_digest_of (param $node i32) (result i32) (local $digest i32) (local $index i32) (local $key i32)
  (if (i32.ne (call $json_tag (local.get $node)) (global.get $JSON_OBJECT))
    (then (global.set $blob_error (call $text_concat (i32.const $s_blob_expected) (call $json_encode (local.get $node)))) (return (i32.const 0))))
  (local.set $digest (call $json_get (local.get $node) (i32.const $s_blob)))
  (if (i32.or (i32.eqz (local.get $digest)) (i32.ne (call $json_tag (local.get $digest)) (global.get $JSON_STRING)))
    (then (global.set $blob_error (call $text_concat (i32.const $s_blob_expected_digest) (call $json_encode (local.get $node)))) (return (i32.const 0))))
  (if (i32.eqz (call $is_digest (call $json_text (local.get $digest))))
    (then (global.set $blob_error (call $text_concat (i32.const $s_blob_expected_digest) (call $json_encode (local.get $node)))) (return (i32.const 0))))
  block $keys
    loop $next
      (br_if $keys (i32.ge_u (local.get $index) (call $json_count (local.get $node))))
      (local.set $key (call $json_key (local.get $node) (local.get $index)))
      (if (i32.eqz (i32.or (call $text_eq (local.get $key) (i32.const $s_blob)) (call $text_eq (local.get $key) (i32.const $s_data))))
        (then (global.set $blob_error (call $text_concat (i32.const $s_blob_holds) (call $json_encode (local.get $node)))) (return (i32.const 0))))
      (local.set $index (i32.add (local.get $index) (i32.const 1)))
      br $next
    end
  end
  (call $json_text (local.get $digest)))
(func $blob_value_from_json (param $node i32) (result i64) (local $digest i32)
  (local.set $digest (call $blob_digest_of (local.get $node)))
  (if (i32.eqz (local.get $digest)) (then (global.set $decode_error (global.get $blob_error)) (return (i64.const 0))))
  (call $text_value (local.get $digest)))
(func $host_data (param $node i32) (result i32) (local $data i32)
  (if (i32.ne (call $json_tag (local.get $node)) (global.get $JSON_OBJECT)) (then (return (i32.const 0))))
  (if (i32.ne (call $json_count (local.get $node)) (i32.const 1)) (then (return (i32.const 0))))
  (local.set $data (call $json_get (local.get $node) (i32.const $s_data)))
  (if (i32.eqz (local.get $data)) (then (return (i32.const 0))))
  (if (i32.ne (call $json_tag (local.get $data)) (global.get $JSON_STRING)) (then (return (i32.const 0))))
  (call $json_text (local.get $data)))
(func $blob_checked (param $tool i32) (param $more i32) (param $output i32) (result i32) (local $data i32) (local $decoded i32) (local $buffer i32)
  (if (i32.ne (call $output_kind (local.get $output)) (global.get $OUTPUT_OK)) (then (return (local.get $output))))
  (local.set $data (call $host_data (call $output_payload (local.get $output))))
  (local.set $buffer (call $buf_new))
  (call $buf_text (local.get $buffer) (local.get $tool))
  (if (i32.eqz (local.get $data))
    (then
      (call $buf_text (local.get $buffer) (i32.const $s_blob_returned))
      (call $json_write (local.get $buffer) (call $output_payload (local.get $output)) (i32.const 0))
      (call $buf_text (local.get $buffer) (i32.const $s_blob_not_blob))
      (return (call $output_new (global.get $OUTPUT_FAILED) (call $buf_finish (local.get $buffer))))))
  (local.set $decoded (call $base64_decode (local.get $data)))
  (if (global.get $base64_error)
    (then
      (call $buf_text (local.get $buffer) (i32.const $s_blob_not_base64))
      (call $buf_text (local.get $buffer) (global.get $base64_error))
      (call $buf_text (local.get $buffer) (i32.const $s_blob_not_base64_close))
      (return (call $output_new (global.get $OUTPUT_FAILED) (call $buf_finish (local.get $buffer))))))
  (if (i32.gt_u (call $text_len (local.get $decoded)) (i32.load offset=4 (local.get $more)))
    (then
      (call $buf_text (local.get $buffer) (i32.const $s_blob_of))
      (call $buf_u64 (local.get $buffer) (i64.extend_i32_u (call $text_len (local.get $decoded))))
      (call $buf_text (local.get $buffer) (i32.const $s_blob_over))
      (call $buf_text (local.get $buffer) (i32.load offset=8 (local.get $more)))
      (call $buf_text (local.get $buffer) (i32.const $s_space_paren))
      (call $buf_u64 (local.get $buffer) (i64.extend_i32_u (i32.load offset=4 (local.get $more))))
      (call $buf_text (local.get $buffer) (i32.const $s_close_paren))
      (return (call $output_new (global.get $OUTPUT_FAILED) (call $buf_finish (local.get $buffer))))))
  (local.get $output))
(func $blob_find (param $digest i32) (result i32) (local $index i32) (local $entry i32)
  block $absent
    loop $next
      (br_if $absent (i32.ge_u (local.get $index) (call $vec_count (global.get $blob_held))))
      (local.set $entry (call $vec_at (global.get $blob_held) (local.get $index)))
      (if (call $text_eq (i32.load (local.get $entry)) (local.get $digest)) (then (return (local.get $entry))))
      (local.set $index (i32.add (local.get $index) (i32.const 1)))
      br $next
    end
  end
  (i32.const 0))
(func $blob_hold (param $digest i32) (param $data i32) (local $entry i32)
  (if (call $blob_find (local.get $digest)) (then (return)))
  (local.set $entry (call $vec_push (global.get $blob_held)))
  (i32.store (local.get $entry) (local.get $digest))
  (i32.store offset=4 (local.get $entry) (local.get $data)))
(func $blob_journaled (param $data i32) (result i32) (local $bytes i32) (local $digest i32) (local $fresh i32) (local $node i32)
  (local.set $bytes (call $base64_decode (local.get $data)))
  (call $assert (i32.eqz (global.get $base64_error)) (i32.const $s_blob_checked_base64))
  (local.set $digest (call $sha256_hex (call $text_bytes (local.get $bytes)) (call $text_len (local.get $bytes))))
  (local.set $fresh (i32.eqz (call $blob_find (local.get $digest))))
  (call $blob_hold (local.get $digest) (local.get $data))
  (if (i32.eqz (local.get $fresh)) (then (return (call $json_one (i32.const $s_blob) (call $json_string (local.get $digest))))))
  (local.set $node (call $json_object (i32.const 2)))
  (call $json_object_set (local.get $node) (i32.const 0) (i32.const $s_blob) (call $json_string (local.get $digest)))
  (call $json_object_set (local.get $node) (i32.const 1) (i32.const $s_data) (call $json_string (local.get $data)))
  (local.get $node))
(func $blob_fail (param $open i32) (param $digest i32) (param $close i32) (param $tail i32) (result i32)
  (global.set $blob_error (call $text_concat (call $wrapped (local.get $open) (local.get $digest) (local.get $close)) (local.get $tail)))
  (i32.const 0))
(func $blob_read (param $node i32) (result i32) (local $digest i32) (local $data i32) (local $bytes i32) (local $hashed i32)
  (local.set $digest (call $blob_digest_of (local.get $node)))
  (if (i32.eqz (local.get $digest)) (then (return (i32.const 0))))
  (local.set $data (call $json_get (local.get $node) (i32.const $s_data)))
  (if (i32.eqz (local.get $data))
    (then
      (if (call $blob_find (local.get $digest)) (then (return (local.get $digest))))
      (return (call $blob_fail (i32.const $s_blob_open) (local.get $digest) (i32.const $s_blob_without) (i32.const $s_empty)))))
  (if (i32.ne (call $json_tag (local.get $data)) (global.get $JSON_STRING))
    (then (return (call $blob_fail (i32.const $s_blob_open) (local.get $digest) (i32.const $s_blob_has_data)
      (call $text_concat (call $json_encode (local.get $data)) (i32.const $s_blob_not_text))))))
  (local.set $bytes (call $base64_decode (call $json_text (local.get $data))))
  (if (global.get $base64_error)
    (then (return (call $blob_fail (i32.const $s_blob_open) (local.get $digest) (i32.const $s_blob_data_not_base64) (global.get $base64_error)))))
  (local.set $hashed (call $sha256_hex (call $text_bytes (local.get $bytes)) (call $text_len (local.get $bytes))))
  (if (i32.eqz (call $text_eq (local.get $hashed) (local.get $digest)))
    (then (return (call $blob_fail (i32.const $s_blob_open) (local.get $digest) (i32.const $s_blob_digest_is) (local.get $hashed)))))
  (call $blob_hold (local.get $digest) (call $json_text (local.get $data)))
  (local.get $digest))
(func $blobs_of_session (result i32) (local $turn i32) (local $log i32) (local $sites i32) (local $index i32) (local $site i32) (local $output i32) (local $buffer i32)
  block $turns_done
    loop $turns
      (br_if $turns_done (i32.ge_u (local.get $turn) (call $vec_count (global.get $turns))))
      (local.set $log (i32.load (call $vec_at (global.get $turns) (local.get $turn))))
      (local.set $sites (call $log_sites (local.get $log)))
      (local.set $index (i32.const 0))
      block $sites_done
        loop $each_site
          (br_if $sites_done (i32.ge_u (local.get $index) (call $vec_count (local.get $sites))))
          (local.set $site (call $vec_at (local.get $sites) (local.get $index)))
          (local.set $output (i32.load offset=16 (local.get $site)))
          (if (i32.eq (i32.load offset=8 (local.get $site)) (global.get $SITE_DONE))
            (then
              (if (i32.and (i32.eq (call $output_kind (local.get $output)) (global.get $OUTPUT_OK))
                    (i32.eq (call $json_tag (call $output_payload (local.get $output))) (global.get $JSON_OBJECT)))
                (then
                  (if (call $tool_blob (call $call_tool (i32.load offset=12 (local.get $site))))
                    (then
                      (if (i32.eqz (call $blob_read (call $output_payload (local.get $output))))
                        (then
                          (local.set $buffer (call $buf_new))
                          (call $buf_text (local.get $buffer) (i32.const $s_blob_result))
                          (call $buf_call_id (local.get $buffer) (i64.add (global.get $turn_first) (i64.extend_i32_u (local.get $turn))) (i64.load (local.get $site)))
                          (call $buf_text (local.get $buffer) (i32.const $s_colon_space))
                          (call $buf_text (local.get $buffer) (global.get $blob_error))
                          (call $corrupt (call $buf_finish (local.get $buffer)))
                          (return (i32.const 0))))))))))
          (local.set $index (i32.add (local.get $index) (i32.const 1)))
          br $each_site
        end
      end
      (local.set $turn (i32.add (local.get $turn) (i32.const 1)))
      br $turns
    end
  end
  (i32.const 1))
(func $blob_missing (param $type i32) (param $value i64) (result i32) (local $kind i32) (local $index i32) (local $count i32) (local $variant i32) (local $missing i32)
  (local.set $kind (call $type_kind (local.get $type)))
  (if (i32.eq (local.get $kind) (global.get $TYPE_BLOB))
    (then
      (if (call $blob_find (call $value_text (local.get $value))) (then (return (i32.const 0))))
      (return (call $value_text (local.get $value)))))
  (if (i32.eq (local.get $kind) (global.get $TYPE_LIST))
    (then
      (local.set $count (call $list_len (local.get $value)))
      block $items_done
        loop $items
          (br_if $items_done (i32.ge_u (local.get $index) (local.get $count)))
          (local.set $missing (call $blob_missing (i32.load offset=4 (local.get $type)) (call $list_get (local.get $value) (local.get $index))))
          (if (local.get $missing) (then (return (local.get $missing))))
          (local.set $index (i32.add (local.get $index) (i32.const 1)))
          br $items
        end
      end
      (return (i32.const 0))))
  (if (i32.lt_u (local.get $kind) (global.get $TYPE_SUM)) (then (return (i32.const 0))))
  (if (i32.gt_u (local.get $kind) (global.get $TYPE_MAYBE)) (then (return (i32.const 0))))
  (if (call $is_call_type (local.get $type)) (then (return (i32.const 0))))
  (local.set $variant (call $sum_variant (local.get $value)))
  (local.set $count (call $variant_fields (local.get $type) (local.get $variant)))
  block $fields_done
    loop $fields
      (br_if $fields_done (i32.ge_u (local.get $index) (local.get $count)))
      (local.set $missing (call $blob_missing (call $variant_field_type (local.get $type) (local.get $variant) (local.get $index)) (call $sum_field (local.get $value) (local.get $index))))
      (if (local.get $missing) (then (return (local.get $missing))))
      (local.set $index (i32.add (local.get $index) (i32.const 1)))
      br $fields
    end
  end
  (i32.const 0))
(func $blobs_load (param $state i32) (result i32) (local $index i32) (local $missing i32)
  (global.set $blob_held (call $vec_new (global.get $BLOB_ENTRY_BYTES)))
  (if (i32.eqz (call $blobs_has)) (then (return (i32.const 1))))
  (call $sha256_check)
  (if (i32.eqz (call $blobs_of_session)) (then (return (i32.const 0))))
  block $done
    loop $next
      (br_if $done (i32.ge_u (local.get $index) (call $program_fields)))
      (local.set $missing (call $blob_missing (i32.load offset=4 (call $program_field (local.get $index)))
        (i64.load (i32.add (local.get $state) (i32.shl (local.get $index) (i32.const 3))))))
      (if (local.get $missing)
        (then
          (call $corrupt (call $wrapped (i32.const $s_blob_state) (local.get $missing) (i32.const $s_blob_state_close)))
          (return (i32.const 0))))
      (local.set $index (i32.add (local.get $index) (i32.const 1)))
      br $next
    end
  end
  (i32.const 1))
(func $blobs_json (result i32) (local $node i32) (local $index i32) (local $entry i32)
  (local.set $node (call $json_object (call $vec_count (global.get $blob_held))))
  block $done
    loop $next
      (br_if $done (i32.ge_u (local.get $index) (call $vec_count (global.get $blob_held))))
      (local.set $entry (call $vec_at (global.get $blob_held) (local.get $index)))
      (call $json_object_set (local.get $node) (local.get $index) (i32.load (local.get $entry)) (call $json_string (i32.load offset=4 (local.get $entry))))
      (local.set $index (i32.add (local.get $index) (i32.const 1)))
      br $next
    end
  end
  (call $json_object_sort (local.get $node))
  (local.get $node))
(func $model_request_blobs (param $request i32) (result i32) (local $buffer i32)
  (if (i32.eqz (call $blobs_has)) (then (return (local.get $request))))
  (local.set $buffer (call $buf_new))
  (call $buf_text (local.get $buffer) (i32.const $s_m_blobs))
  (call $json_write (local.get $buffer) (call $blobs_json) (i32.const 0))
  (call $buf_byte (local.get $buffer) (global.get $ASCII_COMMA))
  (call $buf_bytes (local.get $buffer) (i32.add (call $buf_data (local.get $request)) (i32.const 1)) (i32.sub (call $buf_length (local.get $request)) (i32.const 1)))
  (local.get $buffer))
(func $blobs_evicted (param $total i64) (param $kept i64) (result i64) (local $batch i64) (local $evicted i64)
  (if (i64.le_u (local.get $total) (local.get $kept)) (then (return (i64.const 0))))
  (local.set $batch (i64.shr_u (i64.add (local.get $kept) (i64.const 1)) (i64.const 1)))
  (if (i64.eqz (local.get $batch)) (then (local.set $batch (i64.const 1))))
  (local.set $evicted (i64.mul (i64.div_u (i64.add (i64.sub (local.get $total) (local.get $kept)) (i64.sub (local.get $batch) (i64.const 1))) (local.get $batch)) (local.get $batch)))
  (if (i64.gt_u (local.get $evicted) (local.get $total)) (then (return (local.get $total))))
  (local.get $evicted))
(func $blob_omitted (param $kept i64) (param $digest i32) (result i32) (local $buffer i32)
  (local.set $buffer (call $buf_new))
  (call $buf_text (local.get $buffer) (i32.const $s_blob_omitted))
  (call $buf_u64 (local.get $buffer) (local.get $kept))
  (call $buf_text (local.get $buffer) (i32.const $s_blob_omitted_middle))
  (call $buf_text (local.get $buffer) (local.get $digest))
  (call $buf_text (local.get $buffer) (i32.const $s_blob_omitted_close))
  (call $buf_finish (local.get $buffer)))
(func $keep_blobs (param $list i64) (param $kept i64) (result i64) (local $length i32) (local $index i32) (local $total i64) (local $dropped i64) (local $buffer i32) (local $item i64) (local $left i64)
  (local.set $length (call $list_len (local.get $list)))
  block $counted
    loop $count
      (br_if $counted (i32.ge_u (local.get $index) (local.get $length)))
      (if (i32.eq (call $sum_variant (call $list_get (local.get $list) (local.get $index))) (call $blobs_attached))
        (then (local.set $total (i64.add (local.get $total) (i64.const 1)))))
      (local.set $index (i32.add (local.get $index) (i32.const 1)))
      br $count
    end
  end
  (local.set $dropped (call $blobs_evicted (local.get $total) (local.get $kept)))
  (if (i64.eqz (local.get $dropped)) (then (return (local.get $list))))
  (local.set $buffer (call $list_buffer_new (select (local.get $length) (global.get $LIST_SLOTS_FIRST) (i32.gt_u (local.get $length) (global.get $LIST_SLOTS_FIRST)))))
  (memory.copy (i32.add (local.get $buffer) (i32.const 8)) (i32.add (call $list_buffer (local.get $list)) (i32.const 8)) (i32.shl (local.get $length) (i32.const 3)))
  (i32.store (local.get $buffer) (local.get $length))
  (local.set $list (call $list_pack (local.get $buffer) (local.get $length)))
  (local.set $index (i32.const 0))
  block $replaced
    loop $replace
      (br_if $replaced (i32.ge_u (local.get $index) (local.get $length)))
      (br_if $replaced (i64.eqz (local.get $dropped)))
      (local.set $item (call $list_get (local.get $list) (local.get $index)))
      (if (i32.eq (call $sum_variant (local.get $item)) (call $blobs_attached))
        (then
          (i64.store offset=8 (i32.add (local.get $buffer) (i32.shl (local.get $index) (i32.const 3)))
            (call $sum_2 (call $blobs_result_variant) (call $sum_field (local.get $item) (i32.const 0))
              (call $text_value (call $blob_omitted (local.get $kept) (call $value_text (call $sum_field (local.get $item) (i32.const 1)))))))
          (local.set $dropped (i64.sub (local.get $dropped) (i64.const 1)))))
      (local.set $index (i32.add (local.get $index) (i32.const 1)))
      br $replace
    end
  end
  (local.set $index (i32.const 0))
  block $checked
    loop $check
      (br_if $checked (i32.ge_u (local.get $index) (local.get $length)))
      (if (i32.eq (call $sum_variant (call $list_get (local.get $list) (local.get $index))) (call $blobs_attached))
        (then (local.set $left (i64.add (local.get $left) (i64.const 1)))))
      (local.set $index (i32.add (local.get $index) (i32.const 1)))
      br $check
    end
  end
  (call $assert (i64.le_u (local.get $left) (local.get $kept)) (i32.const $s_blob_keeps))
  (local.get $list))
