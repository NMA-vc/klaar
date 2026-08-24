package main

import "testing"

func TestGreet(t *testing.T) {
    if Greet("test") != "hello test" {
        t.Fatal("unexpected greeting")
    }
}
