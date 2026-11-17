(global $UTF8_CONTINUATION_FIRST i32 (i32.const 0x80))
(global $UTF8_CONTINUATION_END i32 (i32.const 0xc0))
(global $UTF8_LEAD_THREE_FIRST i32 (i32.const 0xe0))
(global $UTF8_LEAD_FOUR_FIRST i32 (i32.const 0xf0))
(global $UTF16_UNITS_PER_TOKEN i64 (i64.const 4))
(global $TRUNCATION_SIDES i64 (i64.const 2))
(global $U64_MAX i64 (i64.const -1))
(global $ASCII_SPACE i32 (i32.const 0x20))
(global $ASCII_TAB i32 (i32.const 0x09))
(global $ASCII_NEWLINE i32 (i32.const 0x0a))
(global $ASCII_FORM_FEED i32 (i32.const 0x0c))
(global $ASCII_RETURN i32 (i32.const 0x0d))
(global $TEXT_CONTAINS i32 (i32.const 0))
(global $TEXT_BEFORE i32 (i32.const 1))
(global $TEXT_TRIM i32 (i32.const 3))
(global $TEXT_WITH_PAYMENT i32 (i32.const 4))
(global $TEXT_JSON_ESCAPE i32 (i32.const 5))
(text $s_payment "payment")
(text $s_truncation_open "\e2\80\a6")
(text $s_truncation_close " tokens truncated\e2\80\a6")
(func $utf8_continuation (param $byte i32) (result i32)
  (i32.and (i32.ge_u (local.get $byte) (global.get $UTF8_CONTINUATION_FIRST)) (i32.lt_u (local.get $byte) (global.get $UTF8_CONTINUATION_END))))
(func $utf8_units (param $byte i32) (result i32)
  (if (call $utf8_continuation (local.get $byte)) (then (return (i32.const 0))))
  (if (i32.ge_u (local.get $byte) (global.get $UTF8_LEAD_FOUR_FIRST)) (then (return (i32.const 2))))
  (i32.const 1))
(func $utf8_width (param $byte i32) (result i32)
  (if (i32.lt_u (local.get $byte) (global.get $UTF8_CONTINUATION_FIRST)) (then (return (i32.const 1))))
  (if (i32.lt_u (local.get $byte) (global.get $UTF8_LEAD_THREE_FIRST)) (then (return (i32.const 2))))
  (if (i32.lt_u (local.get $byte) (global.get $UTF8_LEAD_FOUR_FIRST)) (then (return (i32.const 3))))
  (i32.const 4))
(func $text_byte (param $text i32) (param $at i32) (result i32)
  (i32.load8_u (i32.add (call $text_bytes (local.get $text)) (local.get $at))))
(func $text_utf16_len (param $text i32) (result i64) (local $at i32) (local $units i64)
  block $done
    loop $next
      (br_if $done (i32.ge_u (local.get $at) (call $text_len (local.get $text))))
      (local.set $units (i64.add (local.get $units) (i64.extend_i32_u (call $utf8_units (call $text_byte (local.get $text) (local.get $at))))))
      (local.set $at (i32.add (local.get $at) (i32.const 1)))
      br $next
    end
  end
  (local.get $units))
(func $text_slice (param $text i32) (param $from i32) (param $to i32) (result i32)
  (call $text_from (i32.add (call $text_bytes (local.get $text)) (local.get $from)) (i32.sub (local.get $to) (local.get $from))))
(func $text_find (param $subject i32) (param $separator i32) (result i32) (local $at i32) (local $last i32)
  (if (i32.gt_u (call $text_len (local.get $separator)) (call $text_len (local.get $subject))) (then (return (i32.const -1))))
  (local.set $last (i32.sub (call $text_len (local.get $subject)) (call $text_len (local.get $separator))))
  block $absent
    loop $next
      (br_if $absent (i32.gt_u (local.get $at) (local.get $last)))
      (if (call $bytes_equal (i32.add (call $text_bytes (local.get $subject)) (local.get $at)) (call $text_bytes (local.get $separator)) (call $text_len (local.get $separator)))
        (then (return (local.get $at))))
      (local.set $at (i32.add (local.get $at) (i32.const 1)))
      br $next
    end
  end
  (i32.const -1))
(func $text_whitespace (param $byte i32) (result i32)
  (i32.or (i32.or (i32.eq (local.get $byte) (global.get $ASCII_SPACE)) (i32.eq (local.get $byte) (global.get $ASCII_TAB)))
    (i32.or (i32.or (i32.eq (local.get $byte) (global.get $ASCII_NEWLINE)) (i32.eq (local.get $byte) (global.get $ASCII_FORM_FEED)))
      (i32.eq (local.get $byte) (global.get $ASCII_RETURN)))))
(func $text_trim (param $text i32) (result i32) (local $from i32) (local $to i32)
  (local.set $to (call $text_len (local.get $text)))
  block $front
    loop $next
      (br_if $front (i32.ge_u (local.get $from) (local.get $to)))
      (br_if $front (i32.eqz (call $text_whitespace (call $text_byte (local.get $text) (local.get $from)))))
      (local.set $from (i32.add (local.get $from) (i32.const 1)))
      br $next
    end
  end
  block $back
    loop $next
      (br_if $back (i32.le_u (local.get $to) (local.get $from)))
      (br_if $back (i32.eqz (call $text_whitespace (call $text_byte (local.get $text) (i32.sub (local.get $to) (i32.const 1))))))
      (local.set $to (i32.sub (local.get $to) (i32.const 1)))
      br $next
    end
  end
  (call $text_slice (local.get $text) (local.get $from) (local.get $to)))
(func $text_json_escape (param $text i32) (result i32) (local $buffer i32) (local $quoted i32)
  (local.set $buffer (call $buf_new))
  (call $json_write_string (local.get $buffer) (local.get $text))
  (local.set $quoted (call $buf_finish (local.get $buffer)))
  (call $text_slice (local.get $quoted) (i32.const 1) (i32.sub (call $text_len (local.get $quoted)) (i32.const 1))))
(func $text_op (param $op i32) (param $subject i64) (param $separator i64) (result i64) (local $text i32) (local $at i32)
  (local.set $text (call $value_text (local.get $subject)))
  (if (i32.eq (local.get $op) (global.get $TEXT_TRIM)) (then (return (call $text_value (call $text_trim (local.get $text))))))
  (if (i32.eq (local.get $op) (global.get $TEXT_JSON_ESCAPE)) (then (return (call $text_value (call $text_json_escape (local.get $text))))))
  (if (i32.eq (local.get $op) (global.get $TEXT_WITH_PAYMENT))
    (then (return (call $text_value (call $with_payment (local.get $text) (call $value_text (local.get $separator)))))))
  (local.set $at (call $text_find (local.get $text) (call $value_text (local.get $separator))))
  (if (i32.eq (local.get $op) (global.get $TEXT_CONTAINS)) (then (return (i64.extend_i32_u (i32.ge_s (local.get $at) (i32.const 0))))))
  (if (i32.eq (local.get $op) (global.get $TEXT_BEFORE))
    (then
      (if (i32.lt_s (local.get $at) (i32.const 0)) (then (return (local.get $subject))))
      (return (call $text_value (call $text_slice (local.get $text) (i32.const 0) (local.get $at))))))
  (if (i32.lt_s (local.get $at) (i32.const 0)) (then (return (call $text_value (i32.const $s_empty)))))
  (call $text_value (call $text_slice (local.get $text)
    (i32.add (local.get $at) (call $text_len (call $value_text (local.get $separator)))) (call $text_len (local.get $text)))))
(func $with_payment (param $input i32) (param $payment i32) (result i32) (local $node i32) (local $count i32) (local $index i32) (local $paid i32)
  (local.set $node (call $json_parse (local.get $input)))
  (if (i32.eqz (local.get $node)) (then (return (local.get $input))))
  (if (i32.ne (call $json_tag (local.get $node)) (global.get $JSON_OBJECT)) (then (return (local.get $input))))
  (local.set $count (call $json_count (local.get $node)))
  block $absent
    loop $next
      (br_if $absent (i32.ge_u (local.get $index) (local.get $count)))
      (if (call $text_eq (call $json_key (local.get $node) (local.get $index)) (i32.const $s_payment))
        (then
          (call $json_object_set (local.get $node) (local.get $index) (i32.const $s_payment) (call $json_string (local.get $payment)))
          (return (call $json_encode (local.get $node)))))
      (local.set $index (i32.add (local.get $index) (i32.const 1)))
      br $next
    end
  end
  (local.set $paid (call $json_object (i32.add (local.get $count) (i32.const 1))))
  (local.set $index (i32.const 0))
  block $copied
    loop $copy
      (br_if $copied (i32.ge_u (local.get $index) (local.get $count)))
      (call $json_object_set (local.get $paid) (local.get $index) (call $json_key (local.get $node) (local.get $index)) (call $json_value (local.get $node) (local.get $index)))
      (local.set $index (i32.add (local.get $index) (i32.const 1)))
      br $copy
    end
  end
  (call $json_object_set (local.get $paid) (local.get $count) (i32.const $s_payment) (call $json_string (local.get $payment)))
  (call $json_object_sort (local.get $paid))
  (call $json_encode (local.get $paid)))
(func $u64_saturating_multiply (param $left i64) (param $right i64) (result i64) (local $product i64)
  (local.set $product (i64.mul (local.get $left) (local.get $right)))
  (if (i32.and (i64.ne (local.get $left) (i64.const 0)) (i64.ne (i64.div_u (local.get $product) (local.get $left)) (local.get $right)))
    (then (return (global.get $U64_MAX))))
  (local.get $product))
(func $truncate_head (param $text i32) (param $side i64) (result i32) (local $at i32) (local $units i64)
  block $whole
    loop $next
      (br_if $whole (i32.ge_u (local.get $at) (call $text_len (local.get $text))))
      (local.set $units (i64.add (local.get $units) (i64.extend_i32_u (call $utf8_units (call $text_byte (local.get $text) (local.get $at))))))
      (if (i64.gt_u (local.get $units) (local.get $side)) (then (return (local.get $at))))
      (local.set $at (i32.add (local.get $at) (call $utf8_width (call $text_byte (local.get $text) (local.get $at)))))
      br $next
    end
  end
  (call $text_len (local.get $text)))
(func $truncate_tail (param $text i32) (param $side i64) (result i32) (local $end i32) (local $start i32) (local $units i64)
  (local.set $end (call $text_len (local.get $text)))
  block $whole
    loop $next
      (br_if $whole (i32.eqz (local.get $end)))
      (local.set $start (i32.sub (local.get $end) (i32.const 1)))
      block $lead
        loop $back
          (br_if $lead (i32.eqz (call $utf8_continuation (call $text_byte (local.get $text) (local.get $start)))))
          (local.set $start (i32.sub (local.get $start) (i32.const 1)))
          br $back
        end
      end
      (local.set $units (i64.add (local.get $units) (i64.extend_i32_u (call $utf8_units (call $text_byte (local.get $text) (local.get $start))))))
      (if (i64.gt_u (local.get $units) (local.get $side)) (then (return (local.get $end))))
      (local.set $end (local.get $start))
      br $next
    end
  end
  (i32.const 0))
(func $truncate_middle (param $value i64) (param $tokens i64) (result i64) (local $text i32) (local $total i64) (local $side i64) (local $head i32) (local $tail i32) (local $buffer i32)
  (local.set $text (call $value_text (local.get $value)))
  (local.set $total (i64.div_u (i64.add (call $text_utf16_len (local.get $text)) (i64.sub (global.get $UTF16_UNITS_PER_TOKEN) (i64.const 1))) (global.get $UTF16_UNITS_PER_TOKEN)))
  (if (i64.le_u (local.get $total) (local.get $tokens)) (then (return (local.get $value))))
  (local.set $side (call $u64_saturating_multiply (local.get $tokens) (i64.div_u (global.get $UTF16_UNITS_PER_TOKEN) (global.get $TRUNCATION_SIDES))))
  (local.set $head (call $truncate_head (local.get $text) (local.get $side)))
  (local.set $tail (call $truncate_tail (local.get $text) (local.get $side)))
  (if (i32.lt_u (local.get $tail) (local.get $head)) (then (local.set $tail (local.get $head))))
  (local.set $buffer (call $buf_new))
  (call $buf_bytes (local.get $buffer) (call $text_bytes (local.get $text)) (local.get $head))
  (call $buf_text (local.get $buffer) (i32.const $s_truncation_open))
  (call $buf_u64 (local.get $buffer) (i64.sub (local.get $total) (local.get $tokens)))
  (call $buf_text (local.get $buffer) (i32.const $s_truncation_close))
  (call $buf_bytes (local.get $buffer) (i32.add (call $text_bytes (local.get $text)) (local.get $tail)) (i32.sub (call $text_len (local.get $text)) (local.get $tail)))
  (call $text_value (call $buf_finish (local.get $buffer))))
