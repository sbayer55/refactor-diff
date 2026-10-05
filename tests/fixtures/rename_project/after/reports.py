from users import fetch_user


def summary(cfg, ids):
    timeout = settings.timeout
    users = [fetch_user(i) for i in ids]
    return {"count": len(users), "timeout": timeout}
