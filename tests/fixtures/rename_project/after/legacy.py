from users import get_user


def old_path(user_id):
    return get_user(user_id)


def throwaway_helper(value):
    """Throwaway function for testing PR comments."""
    return value * 2
