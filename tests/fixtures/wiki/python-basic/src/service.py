from .models import User

def greet(user: User) -> str:
    return f"hello {user.name}"
