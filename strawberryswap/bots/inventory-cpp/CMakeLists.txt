cmake_minimum_required(VERSION 3.16)
project(strawberry_searcher LANGUAGES CXX)

set(CMAKE_CXX_STANDARD 20)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_CXX_EXTENSIONS ON)  # unsigned __int128 for 18-decimal amounts (GCC/Clang)
if(NOT CMAKE_BUILD_TYPE)
  set(CMAKE_BUILD_TYPE Release)
endif()

add_library(strawberry INTERFACE)
target_include_directories(strawberry INTERFACE include)
target_compile_options(strawberry INTERFACE -Wall -Wextra -Wpedantic -Wconversion -Wshadow -Werror)

add_executable(searcher_dryrun src/main.cpp)
target_link_libraries(searcher_dryrun PRIVATE strawberry)

enable_testing()
add_executable(selftest tests/selftest.cpp)
target_link_libraries(selftest PRIVATE strawberry)
add_test(NAME selftest COMMAND selftest)
