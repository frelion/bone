import unittest
from datetime import datetime, timezone, timedelta
from retry_after import retry_after_seconds


class RetryAfterTests(unittest.TestCase):
    def test_integer(self):
        self.assertEqual(retry_after_seconds("120"), 120)
        self.assertEqual(retry_after_seconds(5), 5)

    def test_integer_whitespace_and_negative_values(self):
        self.assertEqual(retry_after_seconds("  12  "), 12)
        self.assertEqual(retry_after_seconds("  Mon, 01 Jan 2024 12:01:30 GMT  ", now=datetime(2024, 1, 1, 12, 0, tzinfo=timezone.utc)), 90)
        for value in ("-1", -1, "", "   "):
            with self.subTest(value=value):
                with self.assertRaises(ValueError):
                    retry_after_seconds(value)

    def test_http_date_with_injected_utc_now(self):
        now = datetime(2024, 1, 1, 12, 0, tzinfo=timezone.utc)
        self.assertEqual(
            retry_after_seconds("Mon, 01 Jan 2024 12:01:30 GMT", now=now),
            90,
        )

    def test_http_date_uses_timezone_offsets(self):
        now = datetime(2024, 1, 1, 7, 0, tzinfo=timezone(timedelta(hours=-5)))
        self.assertEqual(
            retry_after_seconds("Mon, 01 Jan 2024 12:00:30 GMT", now=now),
            30,
        )

    def test_expired_http_date_clamps_to_zero(self):
        now = datetime(2024, 1, 1, 12, 0, tzinfo=timezone.utc)
        self.assertEqual(
            retry_after_seconds("Mon, 01 Jan 2024 11:59:00 GMT", now=now),
            0,
        )


if __name__ == "__main__":
    unittest.main()
