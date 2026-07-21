#include "crispembed.h"
#include <node_api.h>

#include <cmath>
#include <cstdint>
#include <cstdlib>
#include <cstring>
#include <string>
#include <vector>

namespace {

constexpr int kEmbeddingDimensions = 384;
constexpr uint32_t kMaxTextCount = 8192;
constexpr size_t kMaxTextBytes = 1024 * 1024;
constexpr size_t kMaxNativeDiagnosticBytes = 4096;

crispembed_context * context = nullptr;
std::string last_native_diagnostic;

void capture_native_diagnostic(const char * message, void *) {
	if (!message) return;
	last_native_diagnostic.assign(message, kMaxNativeDiagnosticBytes);
	while (!last_native_diagnostic.empty() &&
		(last_native_diagnostic.back() == '\n' || last_native_diagnostic.back() == '\r'))
		last_native_diagnostic.pop_back();
}

void throw_native_error(napi_env env, const char * fallback) {
	if (last_native_diagnostic.empty()) {
		napi_throw_error(env, nullptr, fallback);
		return;
	}
	const std::string message = std::string(fallback) + ": " + last_native_diagnostic;
	napi_throw_error(env, nullptr, message.c_str());
}

void throw_error(napi_env env, const char * message) {
	napi_throw_error(env, nullptr, message);
}

bool check(napi_env env, napi_status status, const char * message) {
	if (status == napi_ok) return true;
	throw_error(env, message);
	return false;
}

napi_value undefined_value(napi_env env) {
	napi_value value;
	napi_get_undefined(env, &value);
	return value;
}

bool get_string(napi_env env, napi_value value, std::string * output) {
	size_t length = 0;
	if (!check(env, napi_get_value_string_utf8(env, value, nullptr, 0, &length), "Expected a UTF-8 string")) return false;
	if (length > kMaxTextBytes) {
		throw_error(env, "Embedding text exceeds the maximum supported size");
		return false;
	}
	output->resize(length);
	return check(
		env,
		napi_get_value_string_utf8(env, value, output->data(), length + 1, &length),
		"Could not read a UTF-8 string"
	);
}

void force_cpu() {
#if defined(_WIN32)
	_putenv_s("CRISPEMBED_FORCE_CPU", "1");
#else
	setenv("CRISPEMBED_FORCE_CPU", "1", 1);
#endif
}

napi_value prepare(napi_env env, napi_callback_info info) {
	size_t argc = 1;
	napi_value args[1];
	if (!check(env, napi_get_cb_info(env, info, &argc, args, nullptr, nullptr), "Could not read prepare arguments"))
		return nullptr;
	if (argc != 1) {
		throw_error(env, "prepare requires a model path");
		return nullptr;
	}
	std::string model_path;
	if (!get_string(env, args[0], &model_path)) return nullptr;
	force_cpu();
	last_native_diagnostic.clear();
	crispembed_set_log_callback(capture_native_diagnostic, nullptr);
	if (context) {
		crispembed_free(context);
		context = nullptr;
	}
	context = crispembed_init(model_path.c_str(), 1);
	if (!context) {
		throw_native_error(env, "Could not initialize Bone's local GGUF embedding engine");
		return nullptr;
	}
	return undefined_value(env);
}

napi_value embed(napi_env env, napi_callback_info info) {
	size_t argc = 2;
	napi_value args[2];
	if (!check(env, napi_get_cb_info(env, info, &argc, args, nullptr, nullptr), "Could not read embed arguments"))
		return nullptr;
	if (!context || argc != 2) {
		throw_error(env, "embed requires a prepared engine, a mode, and text values");
		return nullptr;
	}
	int32_t mode = -1;
	if (!check(env, napi_get_value_int32(env, args[0], &mode), "Embedding mode must be an integer") || (mode != 0 && mode != 1))
		return nullptr;
	bool is_array = false;
	if (!check(env, napi_is_array(env, args[1], &is_array), "Could not inspect embedding text values") || !is_array) {
		throw_error(env, "Embedding text values must be an array");
		return nullptr;
	}
	uint32_t count = 0;
	if (!check(env, napi_get_array_length(env, args[1], &count), "Could not read embedding text values")) return nullptr;
	if (count == 0 || count > kMaxTextCount) {
		throw_error(env, "Embedding request has an invalid text count");
		return nullptr;
	}
	std::vector<std::string> texts(count);
	std::vector<const char *> raw_texts(count);
	for (uint32_t index = 0; index < count; index++) {
		napi_value value;
		if (!check(env, napi_get_element(env, args[1], index, &value), "Could not read embedding text")) return nullptr;
		if (!get_string(env, value, &texts[index])) return nullptr;
		raw_texts[index] = texts[index].c_str();
	}
	last_native_diagnostic.clear();
	crispembed_set_prefix(context, mode == 0 ? "query: Find the previous Bone conversation relevant to: " : "passage: ");
	int dimensions = 0;
	const float * vectors = crispembed_encode_batch(context, raw_texts.data(), static_cast<int>(count), &dimensions);
	if (!vectors || dimensions != kEmbeddingDimensions) {
		throw_native_error(env, "Local GGUF embedding engine returned invalid vectors");
		return nullptr;
	}
	const size_t element_count = static_cast<size_t>(count) * static_cast<size_t>(dimensions);
	void * data = nullptr;
	napi_value array_buffer;
	if (!check(env, napi_create_arraybuffer(env, element_count * sizeof(float), &data, &array_buffer), "Could not allocate embedding output"))
		return nullptr;
	std::memcpy(data, vectors, element_count * sizeof(float));
	napi_value typed_array;
	if (!check(
			env,
			napi_create_typedarray(env, napi_float32_array, element_count, array_buffer, 0, &typed_array),
			"Could not create embedding output"
		))
		return nullptr;
	return typed_array;
}

napi_value dispose(napi_env env, napi_callback_info info) {
	if (context) {
		crispembed_free(context);
		context = nullptr;
	}
	return undefined_value(env);
}

napi_value initialize(napi_env env, napi_value exports) {
	napi_property_descriptor properties[] = {
		{ "prepare", nullptr, prepare, nullptr, nullptr, nullptr, napi_default, nullptr },
		{ "embed", nullptr, embed, nullptr, nullptr, nullptr, napi_default, nullptr },
		{ "dispose", nullptr, dispose, nullptr, nullptr, nullptr, napi_default, nullptr },
	};
	if (!check(env, napi_define_properties(env, exports, sizeof(properties) / sizeof(properties[0]), properties), "Could not initialize Bone embedding addon"))
		return nullptr;
	return exports;
}

} // namespace

NAPI_MODULE(NODE_GYP_MODULE_NAME, initialize)
