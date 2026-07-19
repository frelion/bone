cmake_minimum_required(VERSION 3.14)

project(bone_embed LANGUAGES CXX)

set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)

set(BONE_CRISPEMBED_INCLUDE_DIR "" CACHE PATH "CrispEmbed installed include directory")
set(BONE_CRISPEMBED_LIBRARY "" CACHE FILEPATH "CrispEmbed shared or import library")

if(NOT EXISTS "${BONE_CRISPEMBED_INCLUDE_DIR}/crispembed.h")
	message(FATAL_ERROR "BONE_CRISPEMBED_INCLUDE_DIR must contain crispembed.h")
endif()
if(NOT EXISTS "${BONE_CRISPEMBED_LIBRARY}")
	message(FATAL_ERROR "BONE_CRISPEMBED_LIBRARY must point to the CrispEmbed library")
endif()

add_executable(bone-embed bone-embed.cpp)
target_include_directories(bone-embed PRIVATE "${BONE_CRISPEMBED_INCLUDE_DIR}")
target_link_libraries(bone-embed PRIVATE "${BONE_CRISPEMBED_LIBRARY}")

if(APPLE)
	set_target_properties(bone-embed PROPERTIES BUILD_RPATH "@loader_path" INSTALL_RPATH "@loader_path")
elseif(UNIX)
	set_target_properties(bone-embed PROPERTIES BUILD_RPATH "$ORIGIN" INSTALL_RPATH "$ORIGIN")
endif()

install(TARGETS bone-embed RUNTIME DESTINATION .)
