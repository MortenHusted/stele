package targets
func value() {}
func shadow() { shadow := func() {}; shadow() }
func parameter(parameter func()) { parameter() }
func alias() { alias := value; alias() }
func recursive(n int) { if n > 0 { recursive(n - 1) } }
func direct() { value() }
type Runner interface { Run() }
func drive(r Runner) { r.Run() }
type Item struct {}
func (Item) Run() {}
func receiver(item Item) { item.Run() }
var Global = value
func global() { Global() }
type Holder struct { callback func() }
func field(h Holder) { h.callback() }
type Celsius float64
func convert() { _ = Celsius(1) }
func compute() int { return 1 }
var Initialized = compute()
func closureCaller() { f := func() { value() }; f() }
func (Item) Close() { Close := func() {}; Close() }
func Map[T any](x T) T { return x }
func genericExplicit() { _ = Map[int](1) }
func genericImplicit() { _ = Map(2) }
type Outer struct { Item }
func promoted(o Outer) { o.Run() }
