# birds.nix - Combinator definitions from "To Mock a Mockingbird"
# by Raymond Smullyan
#
# Each bird represents a combinator in combinatory logic.
# These are defined as Nix lambda functions.

{ }

: let
  # Identity Bird (I)
  # I x = x
  # The identity bird returns its input unchanged
  I = x: x;

  # Mockingbird (M)
  # M x = x x
  # The mockingbird applies its input to itself
  # Note: M M would diverge (infinite recursion) in Nix
  M = x: x x;

  # Kestrel (K)
  # K x y = x
  # The kestrel always returns the first of two arguments
  # (discarding the second)
  K = x: y: x;

  # Kite (KI)
  # KI x y = y
  # The kite always returns the second of two arguments
  # (discarding the first)
  KI = x: y: y;

  # Bluebird (B)
  # B f g x = f (g x)
  # The bluebird represents function composition
  # B f g = f . g
  B = f: g: x: f (g x);

  # Lark (L)
  # L x y = x (y x)
  # The lark applies the second argument to the first,
  # then applies the result to the first argument
  L = x: y: x (y x);

  # Warbler (W)
  # W f x = f x x
  # The warbler duplicates its argument
  W = f: x: f x x;

  # Starling (S)
  # S f g x = f x (g x)
  # The starling applies x to both f and g, then applies the results
  S = f: g: x: f x (g x);

  # Sage Bird (Y combinator)
  # Y f = f (Y f)
  # The sage bird is the fixed-point combinator
  # It enables recursion in languages without built-in recursion
  # Using Nix's lazy let bindings to avoid infinite recursion
  Y = f: let x = f x; in x;

  # Cardinal (C)
  # C f x y = f y x
  # The cardinal flips the order of arguments to a function
  C = f: x: y: f y x;

  # Vireo (V)
  # V x y z = z x y
  # The vireo is a pair constructor - it applies a function to a pair
  V = x: y: z: z x y;

  # Test cases demonstrating each combinator's behavior

  testI = I "hello";  # evaluates to "hello"

  testK = K "true" "false";  # evaluates to "true"

  testKI = KI "true" "false";  # evaluates to "false"

  testB = B (x: x + 1) (x: x * 2) 5;  # evaluates to 11
  # B f g x = f (g x)
  # B (x -> x+1) (x -> x*2) 5 = (x -> x+1) ((x -> x*2) 5)
  # = (x -> x+1) 10 = 11

  testW_K = W K "anything";  # evaluates to "anything"
  # W K x = K x x = x (K always returns first arg)

  testS_KK = S K K "test";  # evaluates to "test"
  # S K K x = K x (K x) = x
  # So S K K is the identity function

  testC = C (x: y: x + y) 1 2;  # evaluates to 3
  # C f x y = f y x
  # C (\x y -> x+y) 1 2 = (\x y -> x+y) 2 1 = 3

  testV = V "a" "b" (x: y: y);  # evaluates to "b"
  # V x y z = z x y
  # V "a" "b" (\x y -> y) = (\x y -> y) "a" "b" = "b"

in {
  inherit I M K KI B L W S Y C V;
  inherit testI testK testKI testB testW_K testS_KK testC testV;
}
