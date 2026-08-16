// SenseVoice 按需 sidecar：模型加载和推理都在主应用进程之外执行。

#include <chrono>
#include <cstdint>
#include <cstring>
#include <iomanip>
#include <iostream>
#include <optional>
#include <sstream>
#include <string>

#ifdef _WIN32
#include <windows.h>
#endif

#include "sherpa-onnx/c-api/c-api.h"

namespace {

constexpr int kProtocolVersion = 1;
constexpr int kMaximumRecordingSeconds = 60;

std::string EscapeJson(const std::string& value) {
  std::ostringstream output;
  for (unsigned char byte : value) {
    switch (byte) {
      case '"': output << "\\\""; break;
      case '\\': output << "\\\\"; break;
      case '\b': output << "\\b"; break;
      case '\f': output << "\\f"; break;
      case '\n': output << "\\n"; break;
      case '\r': output << "\\r"; break;
      case '\t': output << "\\t"; break;
      default:
        if (byte < 0x20) {
          output << "\\u" << std::hex << std::setw(4) << std::setfill('0')
                 << static_cast<int>(byte) << std::dec;
        } else {
          output << static_cast<char>(byte);
        }
    }
  }
  return output.str();
}

void AppendUtf8(std::string* output, uint32_t code_point) {
  if (code_point <= 0x7F) {
    output->push_back(static_cast<char>(code_point));
  } else if (code_point <= 0x7FF) {
    output->push_back(static_cast<char>(0xC0 | (code_point >> 6)));
    output->push_back(static_cast<char>(0x80 | (code_point & 0x3F)));
  } else {
    output->push_back(static_cast<char>(0xE0 | (code_point >> 12)));
    output->push_back(static_cast<char>(0x80 | ((code_point >> 6) & 0x3F)));
    output->push_back(static_cast<char>(0x80 | (code_point & 0x3F)));
  }
}

// 协议只读取已知字符串字段，未知或损坏转义会返回空值。
std::optional<std::string> ReadJsonString(const std::string& line,
                                          const std::string& key) {
  const std::string marker = "\"" + key + "\"";
  size_t position = line.find(marker);
  if (position == std::string::npos) return std::nullopt;
  position = line.find(':', position + marker.size());
  if (position == std::string::npos) return std::nullopt;
  position = line.find('"', position + 1);
  if (position == std::string::npos) return std::nullopt;

  std::string value;
  for (++position; position < line.size(); ++position) {
    const char current = line[position];
    if (current == '"') return value;
    if (current != '\\') {
      value.push_back(current);
      continue;
    }
    if (++position >= line.size()) return std::nullopt;
    const char escaped = line[position];
    switch (escaped) {
      case '"': value.push_back('"'); break;
      case '\\': value.push_back('\\'); break;
      case '/': value.push_back('/'); break;
      case 'b': value.push_back('\b'); break;
      case 'f': value.push_back('\f'); break;
      case 'n': value.push_back('\n'); break;
      case 'r': value.push_back('\r'); break;
      case 't': value.push_back('\t'); break;
      case 'u': {
        if (position + 4 >= line.size()) return std::nullopt;
        uint32_t code_point = 0;
        for (int offset = 1; offset <= 4; ++offset) {
          const char digit = line[position + offset];
          code_point <<= 4;
          if (digit >= '0' && digit <= '9') code_point += digit - '0';
          else if (digit >= 'a' && digit <= 'f') code_point += digit - 'a' + 10;
          else if (digit >= 'A' && digit <= 'F') code_point += digit - 'A' + 10;
          else return std::nullopt;
        }
        AppendUtf8(&value, code_point);
        position += 4;
        break;
      }
      default: return std::nullopt;
    }
  }
  return std::nullopt;
}

std::optional<std::string> ReadArgument(int argc, char** argv,
                                        const std::string& name) {
  const std::string prefix = "--" + name + "=";
  for (int index = 1; index < argc; ++index) {
    const std::string argument = argv[index];
    if (argument.rfind(prefix, 0) == 0) return argument.substr(prefix.size());
  }
  return std::nullopt;
}

void WriteFailure(const std::optional<std::string>& request_id,
                  const std::string& kind, const std::string& message) {
  std::cout << "{\"event\":\"failed\",\"protocolVersion\":"
            << kProtocolVersion << ",\"requestId\":";
  if (request_id.has_value()) {
    std::cout << "\"" << EscapeJson(*request_id) << "\"";
  } else {
    std::cout << "null";
  }
  std::cout << ",\"error\":{\"kind\":\"" << EscapeJson(kind)
            << "\",\"message\":\"" << EscapeJson(message) << "\"}}"
            << std::endl;
}

double ElapsedMilliseconds(const std::chrono::steady_clock::time_point& start) {
  const auto elapsed = std::chrono::steady_clock::now() - start;
  return std::chrono::duration<double, std::milli>(elapsed).count();
}

}  // namespace

int main(int argc, char** argv) {
#ifdef _WIN32
  // stdin/stdout 固定 UTF-8，避免中文结果受当前控制台代码页影响。
  SetConsoleCP(CP_UTF8);
  SetConsoleOutputCP(CP_UTF8);
#endif

  const auto model = ReadArgument(argc, argv, "model");
  const auto tokens = ReadArgument(argc, argv, "tokens");
  const auto threads_text = ReadArgument(argc, argv, "threads");
  if (!model || !tokens || !threads_text) {
    WriteFailure(std::nullopt, "protocol",
                 "必须提供 --model、--tokens 和 --threads 参数");
    return 2;
  }

  int threads = 0;
  try {
    threads = std::stoi(*threads_text);
  } catch (...) {
    WriteFailure(std::nullopt, "protocol", "threads 必须是整数");
    return 2;
  }
  if (threads < 1 || threads > 64) {
    WriteFailure(std::nullopt, "protocol", "threads 超出 1 到 64 的范围");
    return 2;
  }

  SherpaOnnxOfflineRecognizerConfig config;
  std::memset(&config, 0, sizeof(config));
  config.decoding_method = "greedy_search";
  config.model_config.debug = 0;
  config.model_config.num_threads = threads;
  config.model_config.provider = "cpu";
  config.model_config.tokens = tokens->c_str();
  config.model_config.sense_voice.model = model->c_str();
  config.model_config.sense_voice.language = "zh";
  config.model_config.sense_voice.use_itn = 1;

  const auto load_start = std::chrono::steady_clock::now();
  const SherpaOnnxOfflineRecognizer* recognizer =
      SherpaOnnxCreateOfflineRecognizer(&config);
  if (recognizer == nullptr) {
    WriteFailure(std::nullopt, "model_corrupt", "SenseVoice 模型加载失败");
    return 3;
  }
  std::cout << "{\"event\":\"ready\",\"protocolVersion\":"
            << kProtocolVersion
            << ",\"candidate\":\"sensevoice\",\"loadMs\":" << std::fixed
            << std::setprecision(3) << ElapsedMilliseconds(load_start) << "}"
            << std::endl;

  std::string line;
  while (std::getline(std::cin, line)) {
    const auto operation = ReadJsonString(line, "op");
    if (!operation) {
      WriteFailure(std::nullopt, "protocol", "请求缺少 op 字段");
      continue;
    }
    if (*operation == "shutdown") break;

    const auto request_id = ReadJsonString(line, "requestId");
    const auto wav_path = ReadJsonString(line, "wavPath");
    if (*operation != "transcribe" || !request_id || !wav_path) {
      WriteFailure(request_id, "protocol", "转写请求字段不完整");
      continue;
    }

    const auto inference_start = std::chrono::steady_clock::now();
    const SherpaOnnxWave* wave = SherpaOnnxReadWave(wav_path->c_str());
    if (wave == nullptr) {
      WriteFailure(request_id, "unsupported_audio", "无法读取 WAV 文件");
      continue;
    }
    if (wave->sample_rate <= 0 || wave->num_samples < 0 ||
        wave->num_samples > wave->sample_rate * kMaximumRecordingSeconds) {
      SherpaOnnxFreeWave(wave);
      WriteFailure(request_id, "unsupported_audio", "录音超过 60 秒或格式无效");
      continue;
    }

    const SherpaOnnxOfflineStream* stream =
        SherpaOnnxCreateOfflineStream(recognizer);
    if (stream == nullptr) {
      SherpaOnnxFreeWave(wave);
      WriteFailure(request_id, "sidecar_crashed", "无法创建转写流");
      continue;
    }
    SherpaOnnxAcceptWaveformOffline(stream, wave->sample_rate, wave->samples,
                                    wave->num_samples);
    SherpaOnnxDecodeOfflineStream(recognizer, stream);
    const SherpaOnnxOfflineRecognizerResult* result =
        SherpaOnnxGetOfflineStreamResult(stream);
    const std::string text = result != nullptr && result->text != nullptr
                                 ? result->text
                                 : "";
    const double inference_ms = ElapsedMilliseconds(inference_start);

    std::cout << "{\"event\":\"completed\",\"protocolVersion\":"
              << kProtocolVersion << ",\"requestId\":\""
              << EscapeJson(*request_id) << "\",\"text\":\""
              << EscapeJson(text) << "\",\"inferenceMs\":" << std::fixed
              << std::setprecision(3) << inference_ms << "}" << std::endl;

    if (result != nullptr) SherpaOnnxDestroyOfflineRecognizerResult(result);
    SherpaOnnxDestroyOfflineStream(stream);
    SherpaOnnxFreeWave(wave);
  }

  SherpaOnnxDestroyOfflineRecognizer(recognizer);
  return 0;
}
