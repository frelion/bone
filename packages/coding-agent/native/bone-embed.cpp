#include "crispembed.h"

#include <algorithm>
#include <cstdint>
#include <cstdlib>
#include <cstring>
#include <iostream>
#include <limits>
#include <string>
#include <vector>

namespace {

constexpr uint8_t kPrepare = 1;
constexpr uint8_t kEmbed = 2;
constexpr uint8_t kDispose = 3;
constexpr uint8_t kSuccess = 128;
constexpr uint8_t kError = 129;
constexpr uint32_t kEmbeddingDimensions = 384;
constexpr uint32_t kMaxTextCount = 8192;
constexpr uint32_t kMaxTextBytes = 1024 * 1024;

bool read_exact(void * destination, size_t length) {
	std::cin.read(static_cast<char *>(destination), static_cast<std::streamsize>(length));
	return std::cin.good();
}

bool read_u8(uint8_t & value) {
	return read_exact(&value, sizeof(value));
}

bool read_u32(uint32_t & value) {
	uint8_t bytes[4];
	if (!read_exact(bytes, sizeof(bytes))) return false;
	value = static_cast<uint32_t>(bytes[0]) | (static_cast<uint32_t>(bytes[1]) << 8) |
		(static_cast<uint32_t>(bytes[2]) << 16) | (static_cast<uint32_t>(bytes[3]) << 24);
	return true;
}

bool read_string(std::string & value) {
	uint32_t length = 0;
	if (!read_u32(length) || length > kMaxTextBytes) return false;
	value.resize(length);
	return length == 0 || read_exact(value.data(), length);
}

void write_u8(uint8_t value) {
	std::cout.put(static_cast<char>(value));
}

void write_u32(uint32_t value) {
	const uint8_t bytes[4] = {
		static_cast<uint8_t>(value & 0xff),
		static_cast<uint8_t>((value >> 8) & 0xff),
		static_cast<uint8_t>((value >> 16) & 0xff),
		static_cast<uint8_t>((value >> 24) & 0xff),
	};
	std::cout.write(reinterpret_cast<const char *>(bytes), sizeof(bytes));
}

void write_success(uint32_t id, uint32_t count = 0, uint32_t dimensions = 0, const float * values = nullptr) {
	write_u8(kSuccess);
	write_u32(id);
	write_u32(count);
	write_u32(dimensions);
	if (values && count > 0 && dimensions > 0) {
		const uint64_t value_count = static_cast<uint64_t>(count) * dimensions;
		std::cout.write(reinterpret_cast<const char *>(values), static_cast<std::streamsize>(value_count * sizeof(float)));
	}
	std::cout.flush();
}

void write_error(uint32_t id, const std::string & message) {
	const uint32_t length = static_cast<uint32_t>(std::min<size_t>(message.size(), kMaxTextBytes));
	write_u8(kError);
	write_u32(id);
	write_u32(length);
	if (length > 0) std::cout.write(message.data(), length);
	std::cout.flush();
}

void force_cpu() {
#if defined(_WIN32)
	_putenv_s("CRISPEMBED_FORCE_CPU", "1");
#else
	setenv("CRISPEMBED_FORCE_CPU", "1", 1);
#endif
}

} // namespace

int main() {
	force_cpu();
	crispembed_context * context = nullptr;

	while (true) {
		uint8_t kind = 0;
		uint32_t id = 0;
		if (!read_u8(kind) || !read_u32(id)) break;

		if (kind == kPrepare) {
			std::string model_path;
			if (!read_string(model_path)) {
				write_error(id, "Invalid prepare request");
				continue;
			}
			if (context) {
				crispembed_free(context);
				context = nullptr;
			}
			context = crispembed_init(model_path.c_str(), 1);
			if (!context) {
				write_error(id, "Could not initialize Bone's local GGUF embedding engine");
				continue;
			}
			write_success(id);
			continue;
		}

		if (kind == kEmbed) {
			uint8_t mode = 0;
			uint32_t count = 0;
			if (!context || !read_u8(mode) || !read_u32(count) || count == 0 || count > kMaxTextCount) {
				write_error(id, "Invalid embedding request");
				continue;
			}
			std::vector<std::string> texts(count);
			std::vector<const char *> raw_texts(count);
			bool valid = mode <= 1;
			for (uint32_t index = 0; index < count; index++) {
				if (!read_string(texts[index])) valid = false;
			}
			if (!valid) {
				write_error(id, "Invalid embedding text payload");
				continue;
			}
			for (uint32_t index = 0; index < count; index++) raw_texts[index] = texts[index].c_str();
			crispembed_set_prefix(
				context,
				mode == 0 ? "query: Find the previous Bone conversation relevant to: " : "passage: "
			);
			int dimensions = 0;
			const float * vectors = crispembed_encode_batch(context, raw_texts.data(), static_cast<int>(count), &dimensions);
			if (!vectors || dimensions != static_cast<int>(kEmbeddingDimensions)) {
				write_error(id, "Local GGUF embedding engine returned invalid vectors");
				continue;
			}
			write_success(id, count, kEmbeddingDimensions, vectors);
			continue;
		}

		if (kind == kDispose) {
			if (context) crispembed_free(context);
			write_success(id);
			return 0;
		}

		write_error(id, "Unknown sidecar request");
	}

	if (context) crispembed_free(context);
	return 0;
}
