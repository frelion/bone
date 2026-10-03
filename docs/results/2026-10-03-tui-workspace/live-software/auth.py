def authorize(token, now):
    if not isinstance(token, dict):
        return 401, "unauthorized"
    subject = token.get("subject")
    expiry = token.get("expires_at")
    if not isinstance(subject, str) or not subject:
        return 401, "unauthorized"
    if isinstance(expiry, bool) or not isinstance(expiry, (int, float)):
        return 401, "unauthorized"
    if expiry <= now:
        return 401, "unauthorized"
    return 200, subject
