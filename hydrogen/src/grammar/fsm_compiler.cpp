#include "fsm_grammar.hpp"

#include <cstring>
#include <stdexcept>
#include <algorithm>
#include <iostream>

namespace hydrogen::grammar {

// Token ID assignments for canonical vocabulary tokens
static constexpr uint32_t TOK_PREFIX_NAME_OBJ  = 15;      // "{\"name\": "
static constexpr uint32_t TOK_END_OBJ          = 16;      // "}"
static constexpr uint32_t TOK_START_ARR        = 17;      // "["
static constexpr uint32_t TOK_END_ARR          = 18;      // "]"
static constexpr uint32_t TOK_COMMA_SPACE      = 20;      // ", "
static constexpr uint32_t TOK_QUOTE            = 21;      // "\""

static constexpr uint32_t TOK_TOOL_FS_READ     = 101;     // "\"fs.read\""
static constexpr uint32_t TOK_TOOL_SHELL_RUN   = 102;     // "\"shell.run\""
static constexpr uint32_t TOK_TOOL_TASK_FINISH = 103;     // "\"task.finish\""
static constexpr uint32_t TOK_TOOL_MEM_LOOKUP  = 104;     // "\"memory.lookup\""

static constexpr uint32_t TOK_PROP_PATH        = 201;     // ", \"path\": "
static constexpr uint32_t TOK_PROP_COMMAND     = 202;     // ", \"command\": "
static constexpr uint32_t TOK_PROP_ARGS        = 203;     // ", \"args\": "
static constexpr uint32_t TOK_PROP_RESULT      = 204;     // ", \"result\": "
static constexpr uint32_t TOK_PROP_QUERY       = 205;     // ", \"query\": "
static constexpr uint32_t TOK_PROP_LIMIT       = 206;     // ", \"limit\": "

static constexpr uint32_t TOK_EOS              = 151645;  // "<|im_end|>"
static constexpr uint32_t TOK_EOS_ALT          = 0;       // "<|endoftext|>"

FsmCompiler::FsmCompiler() {
    // Register default Nebula system tool schemas
    ToolSchema fs_read;
    fs_read.name = "fs.read";
    fs_read.properties.push_back({"path", PropertyType::String, true});
    register_tool(fs_read);

    ToolSchema shell_run;
    shell_run.name = "shell.run";
    shell_run.properties.push_back({"command", PropertyType::String, true});
    shell_run.properties.push_back({"args", PropertyType::StringArray, true});
    register_tool(shell_run);

    ToolSchema task_finish;
    task_finish.name = "task.finish";
    task_finish.properties.push_back({"result", PropertyType::String, true});
    register_tool(task_finish);

    ToolSchema memory_lookup;
    memory_lookup.name = "memory.lookup";
    memory_lookup.properties.push_back({"query", PropertyType::String, true});
    memory_lookup.properties.push_back({"limit", PropertyType::Integer, true});
    register_tool(memory_lookup);
}

void FsmCompiler::register_tool(const ToolSchema& schema) {
    schemas_.push_back(schema);
}

void FsmCompiler::initialize_vocabulary(FsmCompiledTable& table) {
    table.token_to_str[TOK_PREFIX_NAME_OBJ]  = "{\"name\": ";
    table.token_to_str[TOK_END_OBJ]          = "}";
    table.token_to_str[TOK_START_ARR]        = "[";
    table.token_to_str[TOK_END_ARR]          = "]";
    table.token_to_str[TOK_COMMA_SPACE]      = ", ";
    table.token_to_str[TOK_QUOTE]            = "\"";

    table.token_to_str[TOK_TOOL_FS_READ]     = "\"fs.read\"";
    table.token_to_str[TOK_TOOL_SHELL_RUN]   = "\"shell.run\"";
    table.token_to_str[TOK_TOOL_TASK_FINISH] = "\"task.finish\"";
    table.token_to_str[TOK_TOOL_MEM_LOOKUP]  = "\"memory.lookup\"";

    table.token_to_str[TOK_PROP_PATH]        = ", \"path\": ";
    table.token_to_str[TOK_PROP_COMMAND]     = ", \"command\": ";
    table.token_to_str[TOK_PROP_ARGS]        = ", \"args\": ";
    table.token_to_str[TOK_PROP_RESULT]      = ", \"result\": ";
    table.token_to_str[TOK_PROP_QUERY]       = ", \"query\": ";
    table.token_to_str[TOK_PROP_LIMIT]       = ", \"limit\": ";

    table.token_to_str[TOK_EOS]              = "<|im_end|>";
    table.token_to_str[TOK_EOS_ALT]          = "<|endoftext|>";

    // File path string tokens
    table.token_to_str[1001] = "crates/nebula-tools/src/lib.rs";
    table.token_to_str[1002] = "hydrogen/include/batcher.hpp";
    table.token_to_str[1003] = "hydrogen/src/grammar/fsm_sampler.cu";
    table.token_to_str[1004] = "docs/ARCHITECTURE.md";
    table.token_to_str[1005] = "Cargo.toml";

    // Shell command tokens
    table.token_to_str[1010] = "cargo test";
    table.token_to_str[1011] = "git status";
    table.token_to_str[1012] = "ninja -C build";
    table.token_to_str[1013] = "pytest -q";

    // Shell arguments
    table.token_to_str[1014] = "--workspace";
    table.token_to_str[1015] = "--locked";
    table.token_to_str[1016] = "-v";

    // Task results
    table.token_to_str[1020] = "all 439 tests passed cleanly";
    table.token_to_str[1021] = "build succeeded with 0 warnings";
    table.token_to_str[1022] = "radix cache hit ratio 94.2%";

    // Memory queries
    table.token_to_str[1030] = "radix tree paged KV cache";
    table.token_to_str[1031] = "split-fuse continuous batching";
    table.token_to_str[1032] = "GPU-fused FSM bitmask sampler";

    // Integers
    table.token_to_str[1040] = "1";
    table.token_to_str[1041] = "5";
    table.token_to_str[1042] = "10";
    table.token_to_str[1043] = "24";
    table.token_to_str[1044] = "64";
    table.token_to_str[1045] = "100";

    for (const auto& [id, str] : table.token_to_str) {
        table.str_to_token[str] = id;
    }
}

bool FsmCompiledTable::is_token_allowed(int state_id, uint32_t token_id) const {
    if (state_id < 0 || static_cast<size_t>(state_id) >= states.size()) {
        return false;
    }
    if (token_id >= vocab_size) {
        return false;
    }
    size_t word_idx = (static_cast<size_t>(state_id) * mask_words) + (token_id >> 5);
    if (word_idx >= bitmasks.size()) {
        return false;
    }
    uint32_t bit = 1u << (token_id & 31);
    return (bitmasks[word_idx] & bit) != 0;
}

int FsmCompiledTable::get_next_state(int state_id, uint32_t token_id) const {
    if (state_id < 0 || static_cast<size_t>(state_id) >= states.size()) {
        return -1;
    }
    const auto& desc = states[state_id];
    for (uint32_t i = 0; i < desc.transition_count; ++i) {
        const auto& tr = transitions[desc.transition_offset + i];
        if (tr.token_id == token_id) {
            return tr.next_state;
        }
    }
    return desc.default_next_state;
}

FsmCompiledTable FsmCompiler::compile() {
    FsmCompiledTable table;
    table.vocab_size = VOCAB_SIZE;
    table.mask_words = MASK_WORDS;
    initialize_vocabulary(table);

    struct BuilderState {
        std::string name;
        std::vector<uint32_t> allowed_tokens;
        std::vector<FsmTransition> transitions;
        int default_next_state = -1;
        bool is_terminal = false;
    };

    std::vector<BuilderState> builder_states;
    auto add_state = [&](const std::string& name) -> int {
        int id = static_cast<int>(builder_states.size());
        BuilderState s;
        s.name = name;
        builder_states.push_back(s);
        return id;
    };

    // State 0: START
    int s_start = add_state("START");
    int s_select_tool = add_state("SELECT_TOOL_NAME");

    builder_states[s_start].allowed_tokens.push_back(TOK_PREFIX_NAME_OBJ);
    builder_states[s_start].transitions.push_back({TOK_PREFIX_NAME_OBJ, s_select_tool});

    // Terminal states
    int s_terminal = add_state("TOOL_TERMINAL");
    builder_states[s_terminal].is_terminal = true;
    int s_done = add_state("DONE");
    builder_states[s_done].is_terminal = true;

    builder_states[s_terminal].allowed_tokens.push_back(TOK_EOS);
    builder_states[s_terminal].transitions.push_back({TOK_EOS, s_done});
    builder_states[s_terminal].allowed_tokens.push_back(TOK_EOS_ALT);
    builder_states[s_terminal].transitions.push_back({TOK_EOS_ALT, s_done});

    // Global Object Closing state
    int s_obj_close = add_state("OBJECT_CLOSE");
    builder_states[s_obj_close].allowed_tokens.push_back(TOK_END_OBJ);
    builder_states[s_obj_close].transitions.push_back({TOK_END_OBJ, s_terminal});

    // Tool 1: fs.read
    // {"name": "fs.read", "path": "<file_path>"}
    {
        int s_fs_after_name = add_state("FS_READ_AFTER_NAME");
        builder_states[s_select_tool].allowed_tokens.push_back(TOK_TOOL_FS_READ);
        builder_states[s_select_tool].transitions.push_back({TOK_TOOL_FS_READ, s_fs_after_name});

        int s_fs_quote_open = add_state("FS_READ_QUOTE_OPEN");
        builder_states[s_fs_after_name].allowed_tokens.push_back(TOK_PROP_PATH);
        builder_states[s_fs_after_name].transitions.push_back({TOK_PROP_PATH, s_fs_quote_open});

        int s_fs_val = add_state("FS_READ_VAL");
        builder_states[s_fs_quote_open].allowed_tokens.push_back(TOK_QUOTE);
        builder_states[s_fs_quote_open].transitions.push_back({TOK_QUOTE, s_fs_val});

        int s_fs_quote_close = add_state("FS_READ_QUOTE_CLOSE");
        std::vector<uint32_t> paths = {1001, 1002, 1003, 1004, 1005};
        for (uint32_t p : paths) {
            builder_states[s_fs_val].allowed_tokens.push_back(p);
            builder_states[s_fs_val].transitions.push_back({p, s_fs_quote_close});
        }

        builder_states[s_fs_quote_close].allowed_tokens.push_back(TOK_QUOTE);
        builder_states[s_fs_quote_close].transitions.push_back({TOK_QUOTE, s_obj_close});
    }

    // Tool 2: shell.run
    // {"name": "shell.run", "command": "<cmd>", "args": ["<arg1>", "<arg2>"]}
    {
        int s_sh_after_name = add_state("SHELL_RUN_AFTER_NAME");
        builder_states[s_select_tool].allowed_tokens.push_back(TOK_TOOL_SHELL_RUN);
        builder_states[s_select_tool].transitions.push_back({TOK_TOOL_SHELL_RUN, s_sh_after_name});

        int s_sh_cmd_quote_open = add_state("SHELL_CMD_QUOTE_OPEN");
        builder_states[s_sh_after_name].allowed_tokens.push_back(TOK_PROP_COMMAND);
        builder_states[s_sh_after_name].transitions.push_back({TOK_PROP_COMMAND, s_sh_cmd_quote_open});

        int s_sh_cmd_val = add_state("SHELL_CMD_VAL");
        builder_states[s_sh_cmd_quote_open].allowed_tokens.push_back(TOK_QUOTE);
        builder_states[s_sh_cmd_quote_open].transitions.push_back({TOK_QUOTE, s_sh_cmd_val});

        int s_sh_cmd_quote_close = add_state("SHELL_CMD_QUOTE_CLOSE");
        std::vector<uint32_t> cmds = {1010, 1011, 1012, 1013};
        for (uint32_t c : cmds) {
            builder_states[s_sh_cmd_val].allowed_tokens.push_back(c);
            builder_states[s_sh_cmd_val].transitions.push_back({c, s_sh_cmd_quote_close});
        }

        int s_sh_args_prop = add_state("SHELL_ARGS_PROP");
        builder_states[s_sh_cmd_quote_close].allowed_tokens.push_back(TOK_QUOTE);
        builder_states[s_sh_cmd_quote_close].transitions.push_back({TOK_QUOTE, s_sh_args_prop});

        int s_sh_arr_open = add_state("SHELL_ARR_OPEN");
        builder_states[s_sh_args_prop].allowed_tokens.push_back(TOK_PROP_ARGS);
        builder_states[s_sh_args_prop].transitions.push_back({TOK_PROP_ARGS, s_sh_arr_open});

        int s_sh_elem1_open = add_state("SHELL_ELEM1_OPEN");
        builder_states[s_sh_arr_open].allowed_tokens.push_back(TOK_START_ARR);
        builder_states[s_sh_arr_open].transitions.push_back({TOK_START_ARR, s_sh_elem1_open});

        int s_sh_elem1_val = add_state("SHELL_ELEM1_VAL");
        builder_states[s_sh_elem1_open].allowed_tokens.push_back(TOK_QUOTE);
        builder_states[s_sh_elem1_open].transitions.push_back({TOK_QUOTE, s_sh_elem1_val});

        int s_sh_elem1_close = add_state("SHELL_ELEM1_CLOSE");
        std::vector<uint32_t> args = {1014, 1015, 1016};
        for (uint32_t a : args) {
            builder_states[s_sh_elem1_val].allowed_tokens.push_back(a);
            builder_states[s_sh_elem1_val].transitions.push_back({a, s_sh_elem1_close});
        }

        int s_sh_elem_sep = add_state("SHELL_ELEM_SEP");
        builder_states[s_sh_elem1_close].allowed_tokens.push_back(TOK_QUOTE);
        builder_states[s_sh_elem1_close].transitions.push_back({TOK_QUOTE, s_sh_elem_sep});

        int s_sh_elem2_open = add_state("SHELL_ELEM2_OPEN");
        builder_states[s_sh_elem_sep].allowed_tokens.push_back(TOK_COMMA_SPACE);
        builder_states[s_sh_elem_sep].transitions.push_back({TOK_COMMA_SPACE, s_sh_elem2_open});

        int s_sh_elem2_val = add_state("SHELL_ELEM2_VAL");
        builder_states[s_sh_elem2_open].allowed_tokens.push_back(TOK_QUOTE);
        builder_states[s_sh_elem2_open].transitions.push_back({TOK_QUOTE, s_sh_elem2_val});

        int s_sh_elem2_close = add_state("SHELL_ELEM2_CLOSE");
        for (uint32_t a : args) {
            builder_states[s_sh_elem2_val].allowed_tokens.push_back(a);
            builder_states[s_sh_elem2_val].transitions.push_back({a, s_sh_elem2_close});
        }

        int s_sh_arr_close = add_state("SHELL_ARR_CLOSE");
        builder_states[s_sh_elem2_close].allowed_tokens.push_back(TOK_QUOTE);
        builder_states[s_sh_elem2_close].transitions.push_back({TOK_QUOTE, s_sh_arr_close});

        builder_states[s_sh_arr_close].allowed_tokens.push_back(TOK_END_ARR);
        builder_states[s_sh_arr_close].transitions.push_back({TOK_END_ARR, s_obj_close});
    }

    // Tool 3: task.finish
    // {"name": "task.finish", "result": "<result_text>"}
    {
        int s_tf_after_name = add_state("TASK_FINISH_AFTER_NAME");
        builder_states[s_select_tool].allowed_tokens.push_back(TOK_TOOL_TASK_FINISH);
        builder_states[s_select_tool].transitions.push_back({TOK_TOOL_TASK_FINISH, s_tf_after_name});

        int s_tf_quote_open = add_state("TASK_FINISH_QUOTE_OPEN");
        builder_states[s_tf_after_name].allowed_tokens.push_back(TOK_PROP_RESULT);
        builder_states[s_tf_after_name].transitions.push_back({TOK_PROP_RESULT, s_tf_quote_open});

        int s_tf_val = add_state("TASK_FINISH_VAL");
        builder_states[s_tf_quote_open].allowed_tokens.push_back(TOK_QUOTE);
        builder_states[s_tf_quote_open].transitions.push_back({TOK_QUOTE, s_tf_val});

        int s_tf_quote_close = add_state("TASK_FINISH_QUOTE_CLOSE");
        std::vector<uint32_t> res = {1020, 1021, 1022};
        for (uint32_t r : res) {
            builder_states[s_tf_val].allowed_tokens.push_back(r);
            builder_states[s_tf_val].transitions.push_back({r, s_tf_quote_close});
        }

        builder_states[s_tf_quote_close].allowed_tokens.push_back(TOK_QUOTE);
        builder_states[s_tf_quote_close].transitions.push_back({TOK_QUOTE, s_obj_close});
    }

    // Tool 4: memory.lookup
    // {"name": "memory.lookup", "query": "<query>", "limit": <int>}
    {
        int s_ml_after_name = add_state("MEM_LOOKUP_AFTER_NAME");
        builder_states[s_select_tool].allowed_tokens.push_back(TOK_TOOL_MEM_LOOKUP);
        builder_states[s_select_tool].transitions.push_back({TOK_TOOL_MEM_LOOKUP, s_ml_after_name});

        int s_ml_quote_open = add_state("MEM_QUERY_QUOTE_OPEN");
        builder_states[s_ml_after_name].allowed_tokens.push_back(TOK_PROP_QUERY);
        builder_states[s_ml_after_name].transitions.push_back({TOK_PROP_QUERY, s_ml_quote_open});

        int s_ml_val = add_state("MEM_QUERY_VAL");
        builder_states[s_ml_quote_open].allowed_tokens.push_back(TOK_QUOTE);
        builder_states[s_ml_quote_open].transitions.push_back({TOK_QUOTE, s_ml_val});

        int s_ml_quote_close = add_state("MEM_QUERY_QUOTE_CLOSE");
        std::vector<uint32_t> queries = {1030, 1031, 1032};
        for (uint32_t q : queries) {
            builder_states[s_ml_val].allowed_tokens.push_back(q);
            builder_states[s_ml_val].transitions.push_back({q, s_ml_quote_close});
        }

        int s_ml_limit_prop = add_state("MEM_LIMIT_PROP");
        builder_states[s_ml_quote_close].allowed_tokens.push_back(TOK_QUOTE);
        builder_states[s_ml_quote_close].transitions.push_back({TOK_QUOTE, s_ml_limit_prop});

        int s_ml_limit_val = add_state("MEM_LIMIT_VAL");
        builder_states[s_ml_limit_prop].allowed_tokens.push_back(TOK_PROP_LIMIT);
        builder_states[s_ml_limit_prop].transitions.push_back({TOK_PROP_LIMIT, s_ml_limit_val});

        std::vector<uint32_t> ints = {1040, 1041, 1042, 1043, 1044, 1045};
        for (uint32_t n : ints) {
            builder_states[s_ml_limit_val].allowed_tokens.push_back(n);
            builder_states[s_ml_limit_val].transitions.push_back({n, s_obj_close});
        }
    }

    // Finalize FsmCompiledTable
    size_t num_states = builder_states.size();
    table.states.resize(num_states);
    table.bitmasks.assign(num_states * MASK_WORDS, 0u);

    for (size_t s = 0; s < num_states; ++s) {
        const auto& bs = builder_states[s];
        auto& desc = table.states[s];

        desc.mask_offset_words = static_cast<uint32_t>(s * MASK_WORDS);
        desc.default_next_state = bs.default_next_state;
        desc.is_terminal = bs.is_terminal;
        desc.transition_offset = static_cast<uint32_t>(table.transitions.size());
        desc.transition_count = static_cast<uint32_t>(bs.transitions.size());

        strncpy_s(desc.name, sizeof(desc.name), bs.name.c_str(), sizeof(desc.name) - 1);

        for (const auto& tr : bs.transitions) {
            table.transitions.push_back(tr);
        }

        uint32_t* mask_ptr = &table.bitmasks[desc.mask_offset_words];
        for (uint32_t tok : bs.allowed_tokens) {
            if (tok < VOCAB_SIZE) {
                mask_ptr[tok >> 5] |= (1u << (tok & 31));
            }
        }
    }

    return table;
}

} // namespace hydrogen::grammar
