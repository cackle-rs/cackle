#include <cstdint>

namespace foo {
namespace bar {
namespace {

int32_t get_value() { return 42; }

}  // namespace
}  // namespace bar
}  // namespace foo

extern "C" {

int32_t cpp_entry_point() {
    return foo::bar::get_value();
}

}
