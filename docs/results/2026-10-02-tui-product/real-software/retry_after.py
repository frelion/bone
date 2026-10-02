from datetime import datetime, timezone
from email.utils import parsedate_to_datetime


def retry_after_seconds(value, now=None):
    """Return the Retry-After delay in seconds for a delta or HTTP-date."""
    if not isinstance(value, str):
        value = str(value)
    value = value.strip()
    if not value:
        raise ValueError("Retry-After value must not be empty")
    try:
        seconds = int(value)
    except ValueError:
        retry_at = parsedate_to_datetime(value)
        if retry_at.tzinfo is None:
            raise ValueError("Retry-After HTTP-date must be timezone-aware")
        current = datetime.now(timezone.utc) if now is None else now
        if current.tzinfo is None or current.utcoffset() is None:
            raise ValueError("now must be timezone-aware")
        return max(0, int((retry_at - current).total_seconds()))
    if seconds < 0:
        raise ValueError("Retry-After seconds must not be negative")
    return seconds
