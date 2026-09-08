// Linux 原生窗口技术验证；没有网络、凭据、RDP 或 CPU 画面读回路径。
#include <gtk/gtk.h>
#include <epoxy/gl.h>
#include <math.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#ifndef __linux__
#error "此技术验证仅支持 Linux 原生构建"
#endif

typedef struct {
    GtkApplication *app;
    GtkWidget *window, *header, *controls, *area;
    GLuint program, vao;
    guint seconds, render_timer, exit_timer;
    guint64 frames, resizes, motions, presses, releases, key_presses, key_releases;
    guint64 focus_enters, focus_leaves, header_actions;
    GLint viewport[4];
    double pointer_x, pointer_y;
    guint errors;
    int backend, expected_backend;
    gboolean use_es, finished, passed;
} Probe;

// 只允许固定范围的驱动信息，不输出键值、键名或用户输入文本。
static void print_gl_string(const char *key, GLenum name) {
    const GLubyte *value = glGetString(name);
    char safe[257];
    size_t length = 0;
    if (value) {
        while (value[length] && length < sizeof(safe) - 1) {
            unsigned char c = value[length];
            safe[length] = (c >= 32 && c <= 126 && c != '"' && c != '\\') ? (char)c : '_';
            ++length;
        }
    }
    safe[length] = '\0';
    printf("{\"gl_%s\":\"%s\"}\n", key, safe);
}

static void fail(Probe *probe, guint code) {
    ++probe->errors;
    printf("{\"error_code\":%u}\n", code);
    fflush(stdout);
}

static GLuint shader(Probe *probe, GLenum type, const char *source) {
    GLuint id = glCreateShader(type);
    glShaderSource(id, 1, &source, NULL);
    glCompileShader(id);
    GLint ok = GL_FALSE;
    glGetShaderiv(id, GL_COMPILE_STATUS, &ok);
    if (!ok) {
        fail(probe, 2);
        glDeleteShader(id);
        return 0;
    }
    return id;
}

static void realized(GtkGLArea *area, gpointer data) {
    Probe *probe = data;
    gtk_gl_area_make_current(area);
    if (gtk_gl_area_get_error(area)) {
        fail(probe, 1);
        return;
    }
    if (gdk_gl_context_get_use_es(gtk_gl_area_get_context(area)) != probe->use_es) {
        fail(probe, 7);
        return;
    }
    print_gl_string("vendor", GL_VENDOR);
    print_gl_string("renderer", GL_RENDERER);
    print_gl_string("version", GL_VERSION);
    const char *prefix = probe->use_es ? "#version 300 es\nprecision highp float;\n" : "#version 330 core\n";
    char *vertex = g_strconcat(prefix,
        "void main(){ vec2 p=vec2(gl_VertexID==1?3.0:-1.0,gl_VertexID==2?3.0:-1.0);"
        "gl_Position=vec4(p,0.0,1.0); }\n", NULL);
    char *fragment = g_strconcat(prefix,
        "out vec4 color; uniform float scale; uniform float frame;"
        "void main(){ vec2 p=gl_FragCoord.xy/max(scale,1.0);"
        "float cell=mod(floor(p.x/32.0)+floor(p.y/32.0),2.0);"
        "vec3 c=mix(vec3(0.06,0.12,0.23),vec3(0.18,0.55,0.70),cell);"
        "if(mod(p.x+frame*2.0,256.0)<3.0)c=vec3(0.95,0.8,0.25);"
        "color=vec4(c,1.0); }\n", NULL);
    GLuint vs = shader(probe, GL_VERTEX_SHADER, vertex);
    GLuint fs = shader(probe, GL_FRAGMENT_SHADER, fragment);
    g_free(vertex);
    g_free(fragment);
    if (!vs || !fs) {
        if (vs) glDeleteShader(vs);
        if (fs) glDeleteShader(fs);
        return;
    }
    probe->program = glCreateProgram();
    glAttachShader(probe->program, vs);
    glAttachShader(probe->program, fs);
    glLinkProgram(probe->program);
    glDeleteShader(vs);
    glDeleteShader(fs);
    GLint linked = GL_FALSE;
    glGetProgramiv(probe->program, GL_LINK_STATUS, &linked);
    if (!linked) {
        fail(probe, 3);
        return;
    }
    glGenVertexArrays(1, &probe->vao);
}

static void unrealized(GtkGLArea *area, gpointer data) {
    Probe *probe = data;
    gtk_gl_area_make_current(area);
    if (!gtk_gl_area_get_error(area)) {
        if (probe->vao) glDeleteVertexArrays(1, &probe->vao);
        if (probe->program) glDeleteProgram(probe->program);
    }
    probe->vao = probe->program = 0;
}

static gboolean rendered(GtkGLArea *area, GdkGLContext *context, gpointer data) {
    (void)context;
    Probe *probe = data;
    if (!probe->program || !probe->vao || gtk_gl_area_get_error(area)) return FALSE;
    // GTK 已设置 GLArea framebuffer 和实际像素 viewport；不绑定窗口 framebuffer 0。
    glGetIntegerv(GL_VIEWPORT, probe->viewport);
    if (glCheckFramebufferStatus(GL_FRAMEBUFFER) != GL_FRAMEBUFFER_COMPLETE) {
        fail(probe, 4);
        return FALSE;
    }
    glDisable(GL_DEPTH_TEST);
    glDisable(GL_BLEND);
    glUseProgram(probe->program);
    glUniform1f(glGetUniformLocation(probe->program, "scale"),
                (float)gtk_widget_get_scale_factor(GTK_WIDGET(area)));
    glUniform1f(glGetUniformLocation(probe->program, "frame"), (float)(probe->frames % 128));
    glBindVertexArray(probe->vao);
    glDrawArrays(GL_TRIANGLES, 0, 3);
    glBindVertexArray(0);
    glUseProgram(0);
    if (glGetError() != GL_NO_ERROR) {
        fail(probe, 5);
        return FALSE;
    }
    ++probe->frames;
    return TRUE;
}

static void resized(GtkGLArea *area, int width, int height, gpointer data) {
    (void)area; (void)width; (void)height;
    ++((Probe *)data)->resizes;
}

static void motion(GtkEventControllerMotion *controller, double x, double y, gpointer data) {
    (void)controller;
    Probe *probe = data;
    ++probe->motions;
    probe->pointer_x = x;
    probe->pointer_y = y;
}

static void pressed(GtkGestureClick *gesture, int count, double x, double y, gpointer data) {
    (void)gesture; (void)count;
    Probe *probe = data;
    ++probe->presses;
    probe->pointer_x = x;
    probe->pointer_y = y;
    gtk_widget_grab_focus(probe->area);
}

static void released(GtkGestureClick *gesture, int count, double x, double y, gpointer data) {
    (void)gesture; (void)count; (void)x; (void)y;
    ++((Probe *)data)->releases;
}

static gboolean key_pressed(GtkEventControllerKey *controller, guint keyval, guint keycode,
                            GdkModifierType state, gpointer data) {
    (void)controller; (void)keyval; (void)keycode; (void)state;
    ++((Probe *)data)->key_presses;
    return FALSE; // 原生 Tab/系统快捷键仍由 GTK/宿主处理。
}

static void key_released(GtkEventControllerKey *controller, guint keyval, guint keycode,
                         GdkModifierType state, gpointer data) {
    (void)controller; (void)keyval; (void)keycode; (void)state;
    ++((Probe *)data)->key_releases;
}

static void focus_enter(GtkEventControllerFocus *controller, gpointer data) {
    (void)controller;
    ++((Probe *)data)->focus_enters;
}

static void focus_leave(GtkEventControllerFocus *controller, gpointer data) {
    (void)controller;
    ++((Probe *)data)->focus_leaves;
}

static void header_action(GtkButton *button, gpointer data) {
    (void)button;
    ++((Probe *)data)->header_actions;
}

static gboolean queue_frame(gpointer data) {
    Probe *probe = data;
    gtk_gl_area_queue_render(GTK_GL_AREA(probe->area));
    return G_SOURCE_CONTINUE;
}

static void report(Probe *probe, gboolean timed) {
    graphene_rect_t header = GRAPHENE_RECT_INIT(0, 0, 0, 0);
    graphene_rect_t area = GRAPHENE_RECT_INIT(0, 0, 0, 0);
    graphene_rect_t controls = GRAPHENE_RECT_INIT(0, 0, 0, 0);
    gboolean mapped = gtk_widget_get_mapped(probe->window);
    gboolean geometry = gtk_widget_compute_bounds(probe->header, probe->window, &header) &&
        gtk_widget_compute_bounds(probe->area, probe->window, &area) &&
        gtk_widget_compute_bounds(probe->controls, probe->window, &controls);
    double center_error = geometry ? fabs((controls.origin.x + controls.size.width / 2.0) -
                                         (header.origin.x + header.size.width / 2.0)) : -1.0;
    gboolean separated = geometry && area.size.width > 0 && area.size.height > 0 &&
        header.size.height > 0 && area.origin.y + 1.0 >= header.origin.y + header.size.height;
    int scale = gtk_widget_get_scale_factor(probe->area);
    gboolean viewport_matches = probe->viewport[0] == 0 && probe->viewport[1] == 0 &&
        probe->viewport[2] == gtk_widget_get_width(probe->area) * scale &&
        probe->viewport[3] == gtk_widget_get_height(probe->area) * scale;
    probe->passed = timed && mapped && separated && viewport_matches && center_error <= 1.0 &&
        probe->frames > 0 && probe->errors == 0 && probe->backend == probe->expected_backend &&
        probe->viewport[2] > 0 && probe->viewport[3] > 0;
    printf("{\"summary\":1,\"passed\":%d,\"timed\":%d,\"backend\":%d,\"mapped\":%d,"
           "\"frames\":%" G_GUINT64_FORMAT ",\"errors\":%u,\"scale\":%d,"
           "\"header_x\":%.2f,\"header_y\":%.2f,\"header_w\":%.2f,\"header_h\":%.2f,"
           "\"content_x\":%.2f,\"content_y\":%.2f,\"content_w\":%.2f,\"content_h\":%.2f,"
           "\"center_error\":%.2f,\"separated\":%d,\"viewport_w\":%d,\"viewport_h\":%d,"
           "\"motions\":%" G_GUINT64_FORMAT ",\"presses\":%" G_GUINT64_FORMAT ","
           "\"releases\":%" G_GUINT64_FORMAT ",\"key_presses\":%" G_GUINT64_FORMAT ","
           "\"key_releases\":%" G_GUINT64_FORMAT ",\"focus_enters\":%" G_GUINT64_FORMAT ","
           "\"focus_leaves\":%" G_GUINT64_FORMAT ",\"header_actions\":%" G_GUINT64_FORMAT ","
           "\"pointer_x\":%.2f,\"pointer_y\":%.2f}\n",
           probe->passed, timed, probe->backend, mapped, probe->frames, probe->errors,
           gtk_widget_get_scale_factor(probe->area), header.origin.x, header.origin.y,
           header.size.width, header.size.height, area.origin.x, area.origin.y,
           area.size.width, area.size.height, center_error, separated,
           probe->viewport[2], probe->viewport[3], probe->motions, probe->presses, probe->releases,
           probe->key_presses, probe->key_releases, probe->focus_enters, probe->focus_leaves,
           probe->header_actions, probe->pointer_x, probe->pointer_y);
    double width = gtk_widget_get_width(probe->area);
    double height = gtk_widget_get_height(probe->area);
    printf("{\"geometry_detail\":1,\"resizes\":%" G_GUINT64_FORMAT
           ",\"content_focus\":%d,\"window_active\":%d,\"pointer_pixel_x\":%.2f,"
           "\"pointer_pixel_y\":%.2f,\"hardware_verified\":0}\n",
           probe->resizes, gtk_widget_has_focus(probe->area),
           gtk_window_is_active(GTK_WINDOW(probe->window)),
           width > 0 ? probe->pointer_x * probe->viewport[2] / width : 0,
           height > 0 ? probe->pointer_y * probe->viewport[3] / height : 0);
    fflush(stdout);
    probe->finished = TRUE;
}

static gboolean finish(gpointer data) {
    Probe *probe = data;
    probe->exit_timer = 0;
    report(probe, TRUE);
    g_application_quit(G_APPLICATION(probe->app));
    return G_SOURCE_REMOVE;
}

static gboolean close_requested(GtkWindow *window, gpointer data) {
    (void)window;
    Probe *probe = data;
    report(probe, FALSE);
    g_application_quit(G_APPLICATION(probe->app));
    return TRUE;
}

static void activate(GtkApplication *app, gpointer data) {
    Probe *probe = data;
    probe->window = gtk_application_window_new(app);
    // 持有自己的引用，避免 application shutdown 提前销毁借用指针。
    g_object_ref(probe->window);
    gtk_window_set_title(GTK_WINDOW(probe->window), "FreeRemoteDesk Linux 窗口技术验证");
    gtk_window_set_default_size(GTK_WINDOW(probe->window), 960, 640);
    GdkDisplay *display = gtk_widget_get_display(probe->window);
    const char *type = G_OBJECT_TYPE_NAME(display);
    probe->backend = strcmp(type, "GdkX11Display") == 0 ? 1 :
                     strcmp(type, "GdkWaylandDisplay") == 0 ? 2 : 0;
    if (probe->backend != probe->expected_backend) fail(probe, 6);
    probe->header = gtk_header_bar_new();
    // 不覆盖 decoration-layout；保留宿主原生按钮和标题栏交互。
    probe->controls = gtk_box_new(GTK_ORIENTATION_HORIZONTAL, 12);
    GtkWidget *label = gtk_label_new("本地测试画面");
    GtkWidget *button = gtk_button_new_with_label("测试操作");
    gtk_widget_set_size_request(button, 88, 44);
    gtk_widget_set_tooltip_text(button, "仅增加本地测试计数，不连接远程设备");
    g_signal_connect(button, "clicked", G_CALLBACK(header_action), probe);
    gtk_box_append(GTK_BOX(probe->controls), label);
    gtk_box_append(GTK_BOX(probe->controls), button);
    gtk_header_bar_set_title_widget(GTK_HEADER_BAR(probe->header), probe->controls);
    gtk_window_set_titlebar(GTK_WINDOW(probe->window), probe->header);
    probe->area = gtk_gl_area_new();
    gtk_widget_set_hexpand(probe->area, TRUE);
    gtk_widget_set_vexpand(probe->area, TRUE);
    gtk_widget_set_focusable(probe->area, TRUE);
    gtk_widget_set_tooltip_text(probe->area, "本地 GL 测试区域；仅统计输入次数");
    gtk_accessible_update_property(GTK_ACCESSIBLE(probe->area), GTK_ACCESSIBLE_PROPERTY_LABEL,
                                   "本地 GL 测试区域", -1);
    gtk_gl_area_set_use_es(GTK_GL_AREA(probe->area), probe->use_es);
    gtk_gl_area_set_required_version(GTK_GL_AREA(probe->area), 3, probe->use_es ? 0 : 3);
    g_signal_connect(probe->area, "realize", G_CALLBACK(realized), probe);
    g_signal_connect(probe->area, "unrealize", G_CALLBACK(unrealized), probe);
    g_signal_connect(probe->area, "render", G_CALLBACK(rendered), probe);
    g_signal_connect(probe->area, "resize", G_CALLBACK(resized), probe);
    GtkEventController *pointer = gtk_event_controller_motion_new();
    g_signal_connect(pointer, "motion", G_CALLBACK(motion), probe);
    gtk_widget_add_controller(probe->area, pointer);
    GtkGesture *click = gtk_gesture_click_new();
    gtk_gesture_single_set_button(GTK_GESTURE_SINGLE(click), 0);
    g_signal_connect(click, "pressed", G_CALLBACK(pressed), probe);
    g_signal_connect(click, "released", G_CALLBACK(released), probe);
    gtk_widget_add_controller(probe->area, GTK_EVENT_CONTROLLER(click));
    GtkEventController *key = gtk_event_controller_key_new();
    g_signal_connect(key, "key-pressed", G_CALLBACK(key_pressed), probe);
    g_signal_connect(key, "key-released", G_CALLBACK(key_released), probe);
    gtk_widget_add_controller(probe->area, key);
    GtkEventController *focus = gtk_event_controller_focus_new();
    g_signal_connect(focus, "enter", G_CALLBACK(focus_enter), probe);
    g_signal_connect(focus, "leave", G_CALLBACK(focus_leave), probe);
    gtk_widget_add_controller(probe->area, focus);
    gtk_window_set_child(GTK_WINDOW(probe->window), probe->area);
    g_signal_connect(probe->window, "close-request", G_CALLBACK(close_requested), probe);
    gtk_window_present(GTK_WINDOW(probe->window));
    probe->render_timer = g_timeout_add(100, queue_frame, probe);
    probe->exit_timer = g_timeout_add(probe->seconds * 1000, finish, probe);
}

int main(int argc, char **argv) {
    Probe probe = { .seconds = 3 };
    for (int i = 1; i < argc; ++i) {
        if (strcmp(argv[i], "--seconds") == 0 && i + 1 < argc) {
            char *end = NULL;
            unsigned long value = strtoul(argv[++i], &end, 10);
            if (!end || *end || value < 1 || value > 300) return 2;
            probe.seconds = (guint)value;
        } else if (strcmp(argv[i], "--expect-backend") == 0 && i + 1 < argc) {
            const char *name = argv[++i];
            probe.expected_backend = strcmp(name, "x11") == 0 ? 1 : strcmp(name, "wayland") == 0 ? 2 : 0;
            if (!probe.expected_backend) return 2;
        } else if (strcmp(argv[i], "--gles") == 0) {
            probe.use_es = TRUE;
        } else {
            fprintf(stderr, "用法：%s --expect-backend x11|wayland [--seconds 1..300] [--gles]\n", argv[0]);
            return 2;
        }
    }
    if (!probe.expected_backend) {
        fputs("必须指定 --expect-backend x11 或 wayland\n", stderr);
        return 2;
    }
    probe.app = gtk_application_new("org.freeremotedesk.NativeShellProbe", G_APPLICATION_NON_UNIQUE);
    g_signal_connect(probe.app, "activate", G_CALLBACK(activate), &probe);
    // 参数已严格处理，不让 GApplication 将探针选项当成桌面文件打开请求。
    char *application_argv[] = { argv[0], NULL };
    int status = g_application_run(G_APPLICATION(probe.app), 1, application_argv);
    if (probe.render_timer) g_source_remove(probe.render_timer);
    if (probe.exit_timer) g_source_remove(probe.exit_timer);
    if (probe.window) {
        gtk_window_destroy(GTK_WINDOW(probe.window));
        g_object_unref(probe.window);
    }
    g_object_unref(probe.app);
    return status == 0 && probe.finished && probe.passed ? 0 : 1;
}
