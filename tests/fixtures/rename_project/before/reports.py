from users import get_user


def summary(cfg, ids):
    timeout = cfg.get("timeout")
    users = [get_user(i) for i in ids]
    return {'count': len(users), 'timeout': timeout}
