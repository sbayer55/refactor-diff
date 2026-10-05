from users import fetch_user


def handle(request, user_id: str):
    user = fetch_user(user_id)
    if user is None or not user.get("active"):
        return 404
    return 200
