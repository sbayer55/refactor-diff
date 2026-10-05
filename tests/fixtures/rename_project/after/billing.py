from users import fetch_user


def charge(cfg, user_id):
    user = fetch_user(user_id)
    timeout = settings.timeout
    return user, timeout


def refund(cfg, user_id):
    return fetch_user(user_id)
