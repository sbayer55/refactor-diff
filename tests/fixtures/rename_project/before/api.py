from users import get_user


def handle(request, user_id: int):
    user = get_user(user_id)
    if user is None:
        return 404
    return 200
