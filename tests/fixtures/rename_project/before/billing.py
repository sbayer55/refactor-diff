from users import get_user


def charge(cfg, user_id):
    user = get_user(user_id)
    timeout = cfg.get("timeout")
    return user, timeout


def refund(cfg, user_id):
    return get_user(user_id)
