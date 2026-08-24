from src.service import greet
from src.models import User

def test_greet():
    assert greet(User("test")) == "hello test"
