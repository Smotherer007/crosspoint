/* crosspoint-fenster – die Crosspoint-Oberflaeche in einem eigenen Fenster,
 * dazu ein Tray-Icon (StatusNotifierItem) fuer den Hintergrundbetrieb.
 *
 * Die Seite selbst ist unveraendert die, die die Engine unter
 * http://127.0.0.1:8770/ ausliefert (web/index.html); dieses Programm zeigt sie
 * mit WebKitGTK in einem GTK3-Fenster. Im Flatpak ersetzt es damit den
 * Host-Browser (flatpak-spawn), auf dem Host bleibt scripts/crosspoint-launch.sh
 * mit dem Browser der Wahl zustaendig.
 *
 * Das Fenster laedt erst, wenn die Engine antwortet: bis dahin wird der
 * TCP-Port abgeklopft (g_usleep, dabei laufen die GTK-Ereignisse weiter).
 * Scheitert das Laden trotzdem, versucht es der load-failed-Callback erneut -
 * so holt sich das Fenster die Seite auch dann, wenn die Engine beim Start
 * noch mit JACK und den virtuellen Geraeten beschaeftigt ist.
 *
 * # Tray
 *
 * Das Icon laeuft ueber StatusNotifierItem (KDE/GNOME-Erweiterung) auf dem
 * Sitzungsbus - GDBus ist mit GTK ohnehin da, zusaetzliche Bibliotheken braucht
 * es nicht. Zum Menue gehoert die Gegenseite com.canonical.dbusmenu, ebenfalls
 * von Hand auf demselben Bus. Befehle im Menue gehen als HTTP an die Engine:
 *
 *   Fenster zeigen         Fenster einblenden (bzw. hervorkommen)
 *   Alles stumm            alle Streifen-Mutes umschalten (POST /api/set)
 *   Letztes Preset laden   POST /api/preset {"action":"load"}
 *   Autostart ein/aus      POST /api/autostart {"enabled":…}
 *   Beenden                Programmende
 *
 * Mit --minimized startet das Programm ohne Fenster: es laedt die Seite im
 * Verborgenen (damit die Engine Daten hat) und zeigt nur das Tray-Icon. Wird
 * das Fenster bei laufendem Tray geschlossen, verschwindet es in den Tray
 * statt das Programm zu beenden.
 *
 *   CROSSPOINT_URL        Adresse der Oberflaeche (Vorgabe http://127.0.0.1:8770/)
 *   CROSSPOINT_DEVTOOLS   1 = Entwicklerwerkzeuge (Ctrl+Shift+I) einschalten
 *   CROSSPOINT_WARTE_S    Sekunden, die auf die Engine gewartet wird (Vorgabe 60)
 *   CROSSPOINT_ICON       Pfad zum Tray-Icon (Vorgabe: Flatpak-Pfad bzw. docs/brand)
 *
 * Schliessen des Fensters ohne Tray beendet das Programm mit 0; das Startskript
 * (scripts/crosspoint-start.sh) stoppt danach die Engine.
 */
#include <gio/gio.h>
#include <gtk/gtk.h>
#include <webkit2/webkit2.h>

#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#define TITEL "Crosspoint"
#define BREITE 1760
#define HOEHE 980
/* Nach so vielen erfolglosen Versuchen gibt das Fenster auf (250 ms je
 * Versuch, also gut eine Minute). */
#define MAX_VERSUCHE 240
/* So oft klopft das Programm an die Engine, damit sie ohne sichtbare Seite
 * nicht in den Leerlauf läuft (die Engine beendet sich sonst). */
#define WACHHALTER_S 10

/* D-Bus: Objektpfade und Menue-IDs. */
#define SNI_PFAD "/StatusNotifierItem"
#define MENUE_PFAD "/MenuBar"
enum {
    ID_FENSTER = 1,
    ID_STUMM = 2,
    ID_PRESET = 3,
    ID_AUTOSTART = 4,
    ID_BEENDEN = 5
};

static GtkWidget *view;
static GtkWidget *fenster;
static const char *url = "http://127.0.0.1:8770/";
static char host[256] = "127.0.0.1";
static guint16 port = 8770;
static int versuche;
static gboolean minimiert = FALSE;   /* --minimized: ohne Fenster starten */
static gboolean tray_angemeldet = FALSE; /* D-Bus-Objekte stehen, Name angefordert */
static gboolean tray_aktiv = FALSE;  /* beim StatusNotifierWatcher angemeldet */
static GDBusConnection *bus;
static guint layout_revision = 1;
static char *busname;

static const char *SNI_XML =
    "<node>"
    " <interface name='org.kde.StatusNotifierItem'>"
    "  <property name='Category' type='s' access='read'/>"
    "  <property name='Id' type='s' access='read'/>"
    "  <property name='Title' type='s' access='read'/>"
    "  <property name='Status' type='s' access='read'/>"
    "  <property name='IconName' type='s' access='read'/>"
    "  <property name='IconPixmap' type='a(iiay)' access='read'/>"
    "  <property name='Menu' type='o' access='read'/>"
    "  <property name='ItemIsMenu' type='b' access='read'/>"
    "  <property name='ToolTip' type='(sa(iiay)ss)' access='read'/>"
    "  <method name='ContextMenu'>"
    "   <arg name='x' type='i' direction='in'/><arg name='y' type='i' direction='in'/>"
    "  </method>"
    "  <method name='Activate'>"
    "   <arg name='x' type='i' direction='in'/><arg name='y' type='i' direction='in'/>"
    "  </method>"
    "  <method name='SecondaryActivate'>"
    "   <arg name='x' type='i' direction='in'/><arg name='y' type='i' direction='in'/>"
    "  </method>"
    "  <method name='Scroll'>"
    "   <arg name='delta' type='i' direction='in'/><arg name='orientation' type='s' direction='in'/>"
    "  </method>"
    " </interface>"
    "</node>";

static const char *MENUE_XML =
    "<node>"
    " <interface name='com.canonical.dbusmenu'>"
    "  <property name='Version' type='u' access='read'/>"
    "  <property name='TextDirection' type='s' access='read'/>"
    "  <property name='Status' type='s' access='read'/>"
    "  <property name='IconThemePath' type='as' access='read'/>"
    "  <method name='GetLayout'>"
    "   <arg name='parentId' type='i' direction='in'/>"
    "   <arg name='recursionDepth' type='i' direction='in'/>"
    "   <arg name='propertyNames' type='as' direction='in'/>"
    "   <arg name='revision' type='u' direction='out'/>"
    "   <arg name='layout' type='(ia{sv}av)' direction='out'/>"
    "  </method>"
    "  <method name='GetGroupProperties'>"
    "   <arg name='ids' type='ai' direction='in'/>"
    "   <arg name='propertyNames' type='as' direction='in'/>"
    "   <arg name='properties' type='a(ia{sv})' direction='out'/>"
    "  </method>"
    "  <method name='GetProperty'>"
    "   <arg name='id' type='i' direction='in'/>"
    "   <arg name='name' type='s' direction='in'/>"
    "   <arg name='value' type='v' direction='out'/>"
    "  </method>"
    "  <method name='Event'>"
    "   <arg name='id' type='i' direction='in'/>"
    "   <arg name='eventId' type='s' direction='in'/>"
    "   <arg name='data' type='v' direction='in'/>"
    "   <arg name='timestamp' type='u' direction='in'/>"
    "  </method>"
    "  <method name='EventGroup'>"
    "   <arg name='events' type='a(isvu)' direction='in'/>"
    "   <arg name='idErrors' type='ai' direction='out'/>"
    "  </method>"
    "  <method name='AboutToShow'>"
    "   <arg name='id' type='i' direction='in'/>"
    "   <arg name='needUpdate' type='b' direction='out'/>"
    "  </method>"
    "  <method name='AboutToShowGroup'>"
    "   <arg name='ids' type='ai' direction='in'/>"
    "   <arg name='updatesNeeded' type='ai' direction='out'/>"
    "   <arg name='idErrors' type='ai' direction='out'/>"
    "  </method>"
    "  <signal name='ItemsPropertiesUpdated'>"
    "   <arg name='updatedProps' type='a(ia{sv})'/>"
    "   <arg name='removedProps' type='a(ias)'/>"
    "  </signal>"
    "  <signal name='LayoutUpdated'>"
    "   <arg name='revision' type='u'/><arg name='parent' type='i'/>"
    "  </signal>"
    "  <signal name='ItemActivationRequested'>"
    "   <arg name='id' type='i'/><arg name='timestamp' type='u'/>"
    "  </signal>"
    " </interface>"
    "</node>";

/* ------------------------------------------------------------------ */
/* HTTP an die Engine                                                  */
/* ------------------------------------------------------------------ */

/* Schickt eine Anfrage an die Engine und liefert den Rumpf (oder NULL).
 * Bewusst winzig: drei Header, Content-Length, Verbindung zu. */
static char *http_anfrage(const char *methode, const char *pfad,
                          const char *koerper, long *status)
{
    GError *fehler = NULL;
    GSocketClient *client = g_socket_client_new();
    g_socket_client_set_timeout(client, 3);
    GSocketConnection *conn =
        g_socket_client_connect_to_host(client, host, port, NULL, &fehler);
    if (!conn) {
        if (fehler) {
            g_printerr("crosspoint-fenster: %s nicht erreichbar (%s)\n", pfad,
                       fehler->message);
            g_error_free(fehler);
        }
        g_object_unref(client);
        return NULL;
    }

    GOutputStream *aus = g_io_stream_get_output_stream(G_IO_STREAM(conn));
    GInputStream *ein = g_io_stream_get_input_stream(G_IO_STREAM(conn));
    gsize laenge = koerper ? strlen(koerper) : 0;
    char *kopf = g_strdup_printf(
        "%s %s HTTP/1.1\r\nHost: %s:%u\r\nConnection: close\r\n"
        "Content-Type: application/json\r\nContent-Length: %" G_GSIZE_FORMAT "\r\n\r\n",
        methode, pfad, host, (unsigned)port, laenge);
    gboolean ok = g_output_stream_write_all(aus, kopf, strlen(kopf), NULL, NULL, &fehler);
    g_free(kopf);
    if (ok && laenge > 0) {
        ok = g_output_stream_write_all(aus, koerper, laenge, NULL, NULL, &fehler);
    }

    GString *antwort = g_string_new(NULL);
    if (ok) {
        char puffer[4096];
        gssize n;
        while ((n = g_input_stream_read(ein, puffer, sizeof puffer, NULL, &fehler)) > 0) {
            g_string_append_len(antwort, puffer, n);
        }
    }
    g_io_stream_close(G_IO_STREAM(conn), NULL, NULL);
    g_object_unref(conn);
    g_object_unref(client);
    if (fehler) {
        g_printerr("crosspoint-fenster: %s: %s\n", pfad, fehler->message);
        g_error_free(fehler);
    }

    char *text = g_string_free(antwort, FALSE);
    if (status) {
        *status = 0;
        if (g_str_has_prefix(text, "HTTP/1.1 ")) {
            *status = strtol(text + 9, NULL, 10);
        }
    }
    char *trenner = strstr(text, "\r\n\r\n");
    char *rumpf = g_strdup(trenner ? trenner + 4 : text);
    g_free(text);
    return rumpf;
}

/* ------------------------------------------------------------------ */
/* Icon                                                                */
/* ------------------------------------------------------------------ */

/* Tray-Icon: im Flatpak das installierte 32x32-PNG, auf dem Host das
 * Markenbild aus docs/brand. Gesucht wird vom Binary aufwaerts, damit der
 * Aufruf nicht vom Arbeitsverzeichnis abhaengt. */
static const char *icon_pfad(void)
{
    static char *gefunden = NULL;
    if (gefunden) {
        return gefunden;
    }
    GPtrArray *kandidaten = g_ptr_array_new_with_free_func(g_free);
    const char *aus_umgebung = g_getenv("CROSSPOINT_ICON");
    if (aus_umgebung && *aus_umgebung) {
        g_ptr_array_add(kandidaten, g_strdup(aus_umgebung));
    }
    g_ptr_array_add(kandidaten, g_strdup(
        "/app/share/icons/hicolor/32x32/apps/de.weppelmann.Crosspoint.png"));
    g_ptr_array_add(kandidaten, g_strdup(
        "/app/share/icons/hicolor/128x128/apps/de.weppelmann.Crosspoint.png"));

    char *exe = g_file_read_link("/proc/self/exe", NULL);
    if (exe) {
        char *dir = g_path_get_dirname(exe);
        char *lauf = g_strdup(dir);
        for (int i = 0; i < 4 && lauf; i++) {
            g_ptr_array_add(kandidaten, g_build_filename(
                lauf, "docs", "brand", "crosspoint-icon-1024.png", NULL));
            char *hoch = g_path_get_dirname(lauf);
            if (g_strcmp0(hoch, lauf) == 0) {
                g_free(hoch);
                break;
            }
            g_free(lauf);
            lauf = hoch;
        }
        g_free(lauf);
        g_free(dir);
        g_free(exe);
    }
    g_ptr_array_add(kandidaten, g_strdup("docs/brand/crosspoint-icon-1024.png"));
    g_ptr_array_add(kandidaten, NULL);

    for (guint i = 0; kandidaten->pdata[i]; i++) {
        const char *p = kandidaten->pdata[i];
        if (g_file_test(p, G_FILE_TEST_IS_REGULAR)) {
            gefunden = g_strdup(p);
            break;
        }
    }
    g_ptr_array_free(kandidaten, TRUE);
    return gefunden;
}

/* IconPixmap: ARGB32 in Netzwerk-Bytefolge, wie StatusNotifierItem es will.
 * Der Umweg ueber Pixmap ist noetig, weil IconName nur im Themes gesucht wird;
 * IconName traegt trotzdem den vollen Pfad (so verlangt). */
static GVariant *icon_pixmap(void)
{
    GVariantBuilder bau;
    g_variant_builder_init(&bau, G_VARIANT_TYPE("a(iiay)"));
    const char *pfad = icon_pfad();
    if (pfad) {
        GError *fehler = NULL;
        GdkPixbuf *bild = gdk_pixbuf_new_from_file(pfad, &fehler);
        if (!bild) {
            if (fehler) {
                g_printerr("crosspoint-fenster: Icon %s nicht lesbar (%s)\n", pfad,
                           fehler->message);
                g_error_free(fehler);
            }
        } else {
            int w = gdk_pixbuf_get_width(bild);
            int h = gdk_pixbuf_get_height(bild);
            int kanaele = gdk_pixbuf_get_n_channels(bild);
            int schritt = gdk_pixbuf_get_rowstride(bild);
            gboolean alpha = gdk_pixbuf_get_has_alpha(bild);
            const guchar *pixel = gdk_pixbuf_read_pixels(bild);
            guint8 *argb = g_malloc0((gsize)w * h * 4);
            for (int y = 0; y < h; y++) {
                for (int x = 0; x < w; x++) {
                    const guchar *q = pixel + (gsize)y * schritt + (gsize)x * kanaele;
                    guint8 *o = argb + ((gsize)y * w + x) * 4;
                    o[0] = alpha ? q[3] : 0xFF;
                    o[1] = q[0];
                    o[2] = q[1];
                    o[3] = q[2];
                }
            }
            g_variant_builder_add(
                &bau, "(ii@ay)", w, h,
                g_variant_new_fixed_array(G_VARIANT_TYPE_BYTE, argb,
                                          (gsize)w * h * 4, 1));
            g_free(argb);
            g_object_unref(bild);
        }
    }
    return g_variant_builder_end(&bau);
}

/* ------------------------------------------------------------------ */
/* Fenster                                                             */
/* ------------------------------------------------------------------ */

static void fenster_zeigen(void)
{
    if (!fenster) {
        return;
    }
    gtk_widget_show_all(fenster);
    gtk_window_deiconify(GTK_WINDOW(fenster));
    gtk_window_present(GTK_WINDOW(fenster));
}

/* "Alles stumm": sind alle Streifen stumm, wieder hoeren; sonst alle stumm.
 * Gelesen wird /api/state, geschrieben je Streifen ueber /api/set - derselbe
 * Weg wie ein Klick in der Oberflaeche, damit der Autosave es mitbekommt. */
static gboolean strips_alle_stumm(const char *json)
{
    const char *anfang = strstr(json, "\"strips\":[");
    if (!anfang) {
        return FALSE;
    }
    const char *ende = strstr(anfang, "\"buses\":[");
    if (!ende) {
        ende = anfang + strlen(anfang);
    }
    int stumm = 0, streifen = 0;
    for (const char *p = anfang; p < ende; p++) {
        if (strncmp(p, "\"mute\":true", 11) == 0) {
            stumm++;
        } else if (strncmp(p, "\"name\":", 7) == 0) {
            streifen++;
        }
    }
    return streifen > 0 && stumm == streifen;
}

static void alles_stumm(void)
{
    char *zustand = http_anfrage("GET", "/api/state", NULL, NULL);
    if (!zustand) {
        return;
    }
    gboolean stumm = strips_alle_stumm(zustand);
    g_free(zustand);
    for (int i = 0; i < 8; i++) {
        char koerper[128];
        g_snprintf(koerper, sizeof koerper,
                   "{\"target\":\"strip\",\"index\":%d,\"param\":\"mute\",\"value\":%d}",
                   i, stumm ? 0 : 1);
        char *antwort = http_anfrage("POST", "/api/set", koerper, NULL);
        g_free(antwort);
    }
    g_print("crosspoint-fenster: Streifen %s\n", stumm ? "wieder hoerbar" : "stumm");
}

static void letztes_preset(void)
{
    char *antwort = http_anfrage("POST", "/api/preset", "{\"action\":\"load\"}", NULL);
    g_print("crosspoint-fenster: Preset geladen%s\n", antwort ? "" : " (fehlgeschlagen)");
    g_free(antwort);
}

static gboolean autostart_ein(void)
{
    char *zustand = http_anfrage("GET", "/api/autostart", NULL, NULL);
    gboolean an = zustand && strstr(zustand, "\"enabled\":true") != NULL;
    g_free(zustand);
    const char *koerper = an ? "{\"enabled\":false}" : "{\"enabled\":true}";
    char *antwort = http_anfrage("POST", "/api/autostart", koerper, NULL);
    g_print("crosspoint-fenster: Autostart %s\n", an ? "aus" : "ein");
    g_free(antwort);
    return !an;
}

/* Menue-Zustand geaendert: die Gastgeber (Plasma, Erweiterungen) holen das
 * Layout neu, wenn die Revision steigt. */
static void menue_geaendert(void)
{
    layout_revision++;
    if (!bus) {
        return;
    }
    GError *fehler = NULL;
    g_dbus_connection_emit_signal(bus, NULL, MENUE_PFAD, "com.canonical.dbusmenu",
                                  "LayoutUpdated", g_variant_new("(ui)", layout_revision, 0),
                                  &fehler);
    if (fehler) {
        g_error_free(fehler);
    }
}

static void aktion(int id)
{
    switch (id) {
    case ID_FENSTER:
        fenster_zeigen();
        break;
    case ID_STUMM:
        alles_stumm();
        menue_geaendert();
        break;
    case ID_PRESET:
        letztes_preset();
        break;
    case ID_AUTOSTART:
        autostart_ein();
        menue_geaendert();
        break;
    case ID_BEENDEN:
        gtk_main_quit();
        break;
    default:
        break;
    }
}

/* ------------------------------------------------------------------ */
/* dbusmenu-Layout                                                     */
/* ------------------------------------------------------------------ */

static GVariant *menue_eintrag(int id, const char *beschriftung, const char *schalter,
                               int zustand)
{
    GVariantBuilder eigenschaften;
    g_variant_builder_init(&eigenschaften, G_VARIANT_TYPE("a{sv}"));
    g_variant_builder_add(&eigenschaften, "{sv}", "label",
                          g_variant_new_string(beschriftung));
    g_variant_builder_add(&eigenschaften, "{sv}", "enabled", g_variant_new_boolean(TRUE));
    g_variant_builder_add(&eigenschaften, "{sv}", "visible", g_variant_new_boolean(TRUE));
    if (schalter) {
        g_variant_builder_add(&eigenschaften, "{sv}", "toggle-type",
                              g_variant_new_string(schalter));
        g_variant_builder_add(&eigenschaften, "{sv}", "toggle-state",
                              g_variant_new_int32(zustand));
    }
    GVariantBuilder kinder;
    g_variant_builder_init(&kinder, G_VARIANT_TYPE("av"));
    return g_variant_new("(ia{sv}av)", id, &eigenschaften, &kinder);
}

static GVariant *menue_layout(void)
{
    /* Zustaende einmal abfragen: Streifen-Stummschaltung und Autostart-Datei. */
    char *zustand = http_anfrage("GET", "/api/state", NULL, NULL);
    gboolean stumm = zustand ? strips_alle_stumm(zustand) : FALSE;
    g_free(zustand);
    char *auto_zustand = http_anfrage("GET", "/api/autostart", NULL, NULL);
    gboolean autostart = auto_zustand && strstr(auto_zustand, "\"enabled\":true") != NULL;
    g_free(auto_zustand);

    GVariantBuilder kinder;
    g_variant_builder_init(&kinder, G_VARIANT_TYPE("av"));
    g_variant_builder_add(&kinder, "v", menue_eintrag(ID_FENSTER, "Fenster zeigen", NULL, 0));
    g_variant_builder_add(&kinder, "v",
                          menue_eintrag(ID_STUMM, "Alles stumm", "checkmark", stumm ? 1 : 0));
    g_variant_builder_add(&kinder, "v",
                          menue_eintrag(ID_PRESET, "Letztes Preset laden", NULL, 0));
    g_variant_builder_add(&kinder, "v", menue_eintrag(ID_AUTOSTART, "Autostart ein/aus",
                                                      "checkmark", autostart ? 1 : 0));
    g_variant_builder_add(&kinder, "v", menue_eintrag(ID_BEENDEN, "Beenden", NULL, 0));

    GVariantBuilder eigenschaften;
    g_variant_builder_init(&eigenschaften, G_VARIANT_TYPE("a{sv}"));
    g_variant_builder_add(&eigenschaften, "{sv}", "children-display",
                          g_variant_new_string("submenu"));
    return g_variant_new("(ia{sv}av)", 0, &eigenschaften, &kinder);
}

/* ------------------------------------------------------------------ */
/* D-Bus-Rueckrufe                                                     */
/* ------------------------------------------------------------------ */

static GVariant *sni_eigenschaft(GDBusConnection *verbindung, const gchar *absender,
                                 const gchar *pfad, const gchar *schnittstelle,
                                 const gchar *name, GError **fehler, gpointer daten)
{
    (void)verbindung;
    (void)absender;
    (void)pfad;
    (void)schnittstelle;
    (void)fehler;
    (void)daten;
    if (g_strcmp0(name, "Category") == 0) {
        return g_variant_new_string("ApplicationStatus");
    }
    if (g_strcmp0(name, "Id") == 0) {
        return g_variant_new_string("de.weppelmann.Crosspoint");
    }
    if (g_strcmp0(name, "Title") == 0) {
        return g_variant_new_string(TITEL);
    }
    if (g_strcmp0(name, "Status") == 0) {
        return g_variant_new_string("Active");
    }
    if (g_strcmp0(name, "IconName") == 0) {
        const char *pfad_icon = icon_pfad();
        return g_variant_new_string(pfad_icon ? pfad_icon : "de.weppelmann.Crosspoint");
    }
    if (g_strcmp0(name, "IconPixmap") == 0) {
        return icon_pixmap();
    }
    if (g_strcmp0(name, "Menu") == 0) {
        return g_variant_new_object_path(MENUE_PFAD);
    }
    if (g_strcmp0(name, "ItemIsMenu") == 0) {
        return g_variant_new_boolean(FALSE);
    }
    if (g_strcmp0(name, "ToolTip") == 0) {
        GVariantBuilder bilder;
        g_variant_builder_init(&bilder, G_VARIANT_TYPE("a(iiay)"));
        return g_variant_new("(s@a(iiay)ss)", "Crosspoint", g_variant_builder_end(&bilder),
                             TITEL, "Audio-Mixer");
    }
    return NULL;
}

static void sni_methode(GDBusConnection *verbindung, const gchar *absender,
                        const gchar *pfad, const gchar *schnittstelle,
                        const gchar *methode, GVariant *parameter,
                        GDBusMethodInvocation *aufruf, gpointer daten)
{
    (void)verbindung;
    (void)absender;
    (void)pfad;
    (void)schnittstelle;
    (void)parameter;
    (void)daten;
    g_print("crosspoint-fenster: Tray %s\n", methode);
    if (g_strcmp0(methode, "Activate") == 0 ||
        g_strcmp0(methode, "SecondaryActivate") == 0) {
        fenster_zeigen();
    }
    /* ContextMenu: der Gastgeber zeigt das dbusmenu unter MENUE_PFAD selbst. */
    g_dbus_method_invocation_return_value(aufruf, NULL);
}

static GVariant *menue_eigenschaft(GDBusConnection *verbindung, const gchar *absender,
                                   const gchar *pfad, const gchar *schnittstelle,
                                   const gchar *name, GError **fehler, gpointer daten)
{
    (void)verbindung;
    (void)absender;
    (void)pfad;
    (void)schnittstelle;
    (void)fehler;
    (void)daten;
    if (g_strcmp0(name, "Version") == 0) {
        return g_variant_new_uint32(3);
    }
    if (g_strcmp0(name, "TextDirection") == 0) {
        return g_variant_new_string("ltr");
    }
    if (g_strcmp0(name, "Status") == 0) {
        return g_variant_new_string("normal");
    }
    if (g_strcmp0(name, "IconThemePath") == 0) {
        GVariantBuilder leer;
        g_variant_builder_init(&leer, G_VARIANT_TYPE("as"));
        return g_variant_builder_end(&leer);
    }
    return NULL;
}

/* Liefert die Eigenschaften eines Eintrags (nur die, die es wirklich gibt). */
static GVariant *eintrag_eigenschaften(int id, const gchar *const *namen)
{
    GVariantBuilder bau;
    g_variant_builder_init(&bau, G_VARIANT_TYPE("a{sv}"));
    const char *beschriftung = NULL;
    const char *schalter = NULL;
    int zustand = 0;
    switch (id) {
    case 0:
        g_variant_builder_add(&bau, "{sv}", "children-display",
                              g_variant_new_string("submenu"));
        break;
    case ID_FENSTER:
        beschriftung = "Fenster zeigen";
        break;
    case ID_STUMM:
        beschriftung = "Alles stumm";
        schalter = "checkmark";
        {
            char *zustand_json = http_anfrage("GET", "/api/state", NULL, NULL);
            zustand = (zustand_json && strips_alle_stumm(zustand_json)) ? 1 : 0;
            g_free(zustand_json);
        }
        break;
    case ID_PRESET:
        beschriftung = "Letztes Preset laden";
        break;
    case ID_AUTOSTART:
        beschriftung = "Autostart ein/aus";
        schalter = "checkmark";
        {
            char *auto_json = http_anfrage("GET", "/api/autostart", NULL, NULL);
            zustand = (auto_json && strstr(auto_json, "\"enabled\":true")) ? 1 : 0;
            g_free(auto_json);
        }
        break;
    case ID_BEENDEN:
        beschriftung = "Beenden";
        break;
    default:
        return NULL;
    }
    gboolean gefragt = (namen == NULL || namen[0] == NULL);
    if (beschriftung) {
        if (gefragt || g_strv_contains(namen, "label")) {
            g_variant_builder_add(&bau, "{sv}", "label",
                                  g_variant_new_string(beschriftung));
        }
        if (gefragt || g_strv_contains(namen, "enabled")) {
            g_variant_builder_add(&bau, "{sv}", "enabled", g_variant_new_boolean(TRUE));
        }
        if (gefragt || g_strv_contains(namen, "visible")) {
            g_variant_builder_add(&bau, "{sv}", "visible", g_variant_new_boolean(TRUE));
        }
        if (schalter) {
            if (gefragt || g_strv_contains(namen, "toggle-type")) {
                g_variant_builder_add(&bau, "{sv}", "toggle-type",
                                      g_variant_new_string(schalter));
            }
            if (gefragt || g_strv_contains(namen, "toggle-state")) {
                g_variant_builder_add(&bau, "{sv}", "toggle-state",
                                      g_variant_new_int32(zustand));
            }
        }
    }
    return g_variant_builder_end(&bau);
}

static void menue_methode(GDBusConnection *verbindung, const gchar *absender,
                          const gchar *pfad, const gchar *schnittstelle,
                          const gchar *methode, GVariant *parameter,
                          GDBusMethodInvocation *aufruf, gpointer daten)
{
    (void)verbindung;
    (void)absender;
    (void)pfad;
    (void)schnittstelle;
    (void)daten;

    if (g_strcmp0(methode, "GetLayout") == 0) {
        gint32 parent = 0, tiefe = 0;
        GVariant *namen = NULL;
        g_variant_get(parameter, "(ii@as)", &parent, &tiefe, &namen);
        g_variant_unref(namen);
        (void)tiefe;
        GVariant *wurzel = (parent == 0) ? menue_layout() : NULL;
        if (!wurzel) {
            GVariantBuilder eigenschaften;
            g_variant_builder_init(&eigenschaften, G_VARIANT_TYPE("a{sv}"));
            GVariantBuilder kinder;
            g_variant_builder_init(&kinder, G_VARIANT_TYPE("av"));
            wurzel = g_variant_new("(ia{sv}av)", parent, &eigenschaften, &kinder);
        }
        g_dbus_method_invocation_return_value(
            aufruf, g_variant_new("(u@(ia{sv}av))", layout_revision, wurzel));
        return;
    }

    if (g_strcmp0(methode, "GetGroupProperties") == 0) {
        GVariant *ids = NULL, *namen = NULL;
        g_variant_get(parameter, "(@ai@as)", &ids, &namen);
        gsize anzahl = g_variant_n_children(ids);
        gchar **listennamen = g_variant_dup_strv(namen, NULL);
        GVariantBuilder bau;
        g_variant_builder_init(&bau, G_VARIANT_TYPE("a(ia{sv})"));
        for (gsize i = 0; i < anzahl; i++) {
            gint32 id = 0;
            g_variant_get_child(ids, i, "i", &id);
            GVariant *eigenschaften = eintrag_eigenschaften(id, (const gchar *const *)listennamen);
            if (eigenschaften) {
                g_variant_builder_add(&bau, "(i@a{sv})", id, eigenschaften);
            }
        }
        g_strfreev(listennamen);
        g_variant_unref(ids);
        g_variant_unref(namen);
        g_dbus_method_invocation_return_value(aufruf, g_variant_new("(a(ia{sv}))", &bau));
        return;
    }

    if (g_strcmp0(methode, "GetProperty") == 0) {
        gint32 id = 0;
        const gchar *name = NULL;
        g_variant_get(parameter, "(i&s)", &id, &name);
        GVariant *eigenschaften = eintrag_eigenschaften(id, NULL);
        GVariant *wert = g_variant_lookup_value(eigenschaften, name, NULL);
        if (!wert) {
            wert = g_variant_new_string("");
        }
        g_variant_unref(eigenschaften);
        g_dbus_method_invocation_return_value(aufruf, g_variant_new("(v)", wert));
        return;
    }

    if (g_strcmp0(methode, "Event") == 0) {
        gint32 id = 0;
        const gchar *ereignis = NULL;
        GVariant *datenwert = NULL;
        guint32 zeit = 0;
        g_variant_get(parameter, "(i&svu)", &id, &ereignis, &datenwert, &zeit);
        gboolean klick = g_strcmp0(ereignis, "clicked") == 0;
        g_variant_unref(datenwert);
        if (klick) {
            aktion(id);
        }
        g_dbus_method_invocation_return_value(aufruf, NULL);
        return;
    }

    if (g_strcmp0(methode, "EventGroup") == 0) {
        GVariantBuilder leer;
        g_variant_builder_init(&leer, G_VARIANT_TYPE("ai"));
        g_dbus_method_invocation_return_value(aufruf,
                                              g_variant_new("(@ai)", g_variant_builder_end(&leer)));
        return;
    }

    if (g_strcmp0(methode, "AboutToShow") == 0) {
        g_dbus_method_invocation_return_value(aufruf, g_variant_new("(b)", TRUE));
        return;
    }

    if (g_strcmp0(methode, "AboutToShowGroup") == 0) {
        GVariantBuilder leere_ids, leere_fehler;
        g_variant_builder_init(&leere_ids, G_VARIANT_TYPE("ai"));
        g_variant_builder_init(&leere_fehler, G_VARIANT_TYPE("ai"));
        g_dbus_method_invocation_return_value(
            aufruf, g_variant_new("(aiai)", &leere_ids, &leere_fehler));
        return;
    }

    g_dbus_method_invocation_return_dbus_error(aufruf, "org.freedesktop.DBus.Error.UnknownMethod",
                                               "unbekannte Methode");
}

/* ------------------------------------------------------------------ */
/* Tray anmelden                                                       */
/* ------------------------------------------------------------------ */

static void bei_namenserwerb(GDBusConnection *verbindung, const gchar *name, gpointer daten)
{
    (void)daten;
    GError *fehler = NULL;
    if (!g_dbus_connection_call_sync(
            verbindung, "org.kde.StatusNotifierWatcher", "/StatusNotifierWatcher",
            "org.kde.StatusNotifierWatcher", "RegisterStatusNotifierItem",
            g_variant_new("(s)", name), NULL, G_DBUS_CALL_FLAGS_NONE, 3000, NULL, &fehler)) {
        g_printerr("crosspoint-fenster: StatusNotifierWatcher nimmt das Icon nicht an (%s)\n",
                   fehler ? fehler->message : "?");
        if (fehler) {
            g_error_free(fehler);
        }
        /* Ohne Tray waere ein verstecktes Fenster nicht mehr erreichbar:
         * dann lieber zeigen. */
        if (fenster && !gtk_widget_get_visible(fenster)) {
            fenster_zeigen();
        }
        return;
    }
    bus = verbindung;
    tray_aktiv = TRUE;
    g_print("crosspoint-fenster: Tray-Icon angemeldet als %s\n", name);
}

static void bei_namensverlust(GDBusConnection *verbindung, const gchar *name, gpointer daten)
{
    (void)verbindung;
    (void)name;
    (void)daten;
    tray_aktiv = FALSE;
    g_printerr("crosspoint-fenster: Tray-Name verloren\n");
}

/* Meldet das StatusNotifierItem an. Schlaegt etwas fehl, laeuft das Programm
 * weiter - dann eben ohne Tray (das Fenster bleibt erreichbar). */
static gboolean tray_starten(void)
{
    GError *fehler = NULL;
    GDBusConnection *verbindung = g_bus_get_sync(G_BUS_TYPE_SESSION, NULL, &fehler);
    if (!verbindung) {
        g_printerr("crosspoint-fenster: kein Sitzungsbus (%s)\n",
                   fehler ? fehler->message : "?");
        if (fehler) {
            g_error_free(fehler);
        }
        return FALSE;
    }

    GDBusNodeInfo *sni = g_dbus_node_info_new_for_xml(SNI_XML, &fehler);
    GDBusNodeInfo *menue = g_dbus_node_info_new_for_xml(MENUE_XML, &fehler);
    if (!sni || !menue) {
        g_printerr("crosspoint-fenster: D-Bus-Schnittstellen nicht lesbar (%s)\n",
                   fehler ? fehler->message : "?");
        if (fehler) {
            g_error_free(fehler);
        }
        if (sni) {
            g_dbus_node_info_unref(sni);
        }
        if (menue) {
            g_dbus_node_info_unref(menue);
        }
        g_object_unref(verbindung);
        return FALSE;
    }

    static const GDBusInterfaceVTable sni_tabelle = {
        .method_call = sni_methode,
        .get_property = sni_eigenschaft,
    };
    static const GDBusInterfaceVTable menue_tabelle = {
        .method_call = menue_methode,
        .get_property = menue_eigenschaft,
    };
    guint sni_id = g_dbus_connection_register_object(
        verbindung, SNI_PFAD, sni->interfaces[0], &sni_tabelle, NULL, NULL, &fehler);
    guint menue_id = g_dbus_connection_register_object(
        verbindung, MENUE_PFAD, menue->interfaces[0], &menue_tabelle, NULL, NULL, &fehler);
    g_dbus_node_info_unref(sni);
    g_dbus_node_info_unref(menue);
    if (sni_id == 0 || menue_id == 0) {
        g_printerr("crosspoint-fenster: D-Bus-Objekte nicht anlegbar (%s)\n",
                   fehler ? fehler->message : "?");
        if (fehler) {
            g_error_free(fehler);
        }
        g_object_unref(verbindung);
        return FALSE;
    }

    busname = g_strdup_printf("org.kde.StatusNotifierItem-%d-1", (int)getpid());
    g_bus_own_name_on_connection(verbindung, busname, G_BUS_NAME_OWNER_FLAGS_NONE,
                                 bei_namenserwerb, bei_namensverlust, NULL, NULL);
    g_object_unref(verbindung);
    /* Der Namenserwerb und die Anmeldung beim Watcher laufen asynchron; ob es
     * geklappt hat, steht danach in tray_aktiv. Fuer die Entscheidung
     * "Fenster zeigen oder nicht" genuegt, dass die Objekte stehen. */
    tray_angemeldet = TRUE;
    return TRUE;
}

/* ------------------------------------------------------------------ */
/* Fenster bauen und laden                                             */
/* ------------------------------------------------------------------ */

/* Ein offener TCP-Port ist das Zeichen, dass die Engine laeuft: HTTP selbst
 * prueft das Startskript, hier genuegt die Verbindung. */
static gboolean tcp_erreichbar(void)
{
    GSocketClient *client = g_socket_client_new();
    GError *fehler = NULL;
    g_socket_client_set_timeout(client, 1);
    GSocketConnection *conn =
        g_socket_client_connect_to_host(client, host, port, NULL, &fehler);
    if (fehler) {
        g_error_free(fehler);
    }
    if (conn) {
        g_object_unref(conn);
    }
    g_object_unref(client);
    return conn != NULL;
}

static void seite_laden(void)
{
    webkit_web_view_load_uri(WEBKIT_WEB_VIEW(view), url);
}

/* Nach einem Fehlversuch: neu laden, sobald die Engine antwortet. Solange sie
 * schweigt, laeuft der Timer weiter, ohne WebKit mit Fehlerseiten zu fluten. */
static gboolean lade_fehler_erneut(gpointer daten)
{
    (void)daten;
    if (tcp_erreichbar()) {
        seite_laden();
        return G_SOURCE_REMOVE;
    }
    if (++versuche >= MAX_VERSUCHE) {
        g_printerr("crosspoint-fenster: Engine antwortet nicht auf %s:%u\n", host,
                   (unsigned)port);
        /* Ohne Tray gibt es sonst keinen Weg mehr weiter. */
        if (!tray_aktiv) {
            gtk_main_quit();
        }
        return G_SOURCE_REMOVE;
    }
    return G_SOURCE_CONTINUE;
}

/* load-failed: WebKit wuerde sonst seine Fehlerseite zeigen. */
static gboolean lade_fehler(WebKitWebView *wv, WebKitLoadEvent ereignis,
                            const gchar *uri, GError *fehler, gpointer daten)
{
    (void)wv;
    (void)ereignis;
    (void)uri;
    (void)daten;
    if (++versuche == 1 || versuche % 8 == 0) {
        g_printerr("crosspoint-fenster: warte auf die Engine (%s), Versuch %d\n",
                   fehler ? fehler->message : "?", versuche);
    }
    if (versuche >= MAX_VERSUCHE) {
        g_printerr("crosspoint-fenster: %s laesst sich nicht laden (%s)\n", url,
                   fehler ? fehler->message : "unbekannt");
        if (!tray_aktiv) {
            gtk_main_quit();
        }
        return TRUE;
    }
    g_timeout_add(250, lade_fehler_erneut, NULL);
    return TRUE;
}

static void lade_status(WebKitWebView *wv, WebKitLoadEvent ereignis, gpointer daten)
{
    (void)wv;
    (void)daten;
    if (ereignis == WEBKIT_LOAD_FINISHED) {
        versuche = 0;
        g_print("crosspoint-fenster: Oberflaeche geladen (%s)\n", url);
    }
}

/* Kontextmenue aus: die Oberflaeche bringt eigene Bedienelemente mit, das
 * Browser-Menue (Neu laden, Seite speichern, ...) stoert nur. */
static gboolean kein_kontextmenue(WebKitWebView *wv, WebKitContextMenu *menue,
                                  GdkEvent *ereignis, WebKitHitTestResult *treffer,
                                  gpointer daten)
{
    (void)wv;
    (void)menue;
    (void)ereignis;
    (void)treffer;
    (void)daten;
    return TRUE;
}

/* Schliessen: mit Tray verschwindet nur das Fenster (das Programm laeuft
 * weiter und ist ueber das Icon erreichbar), ohne Tray endet das Programm -
 * das Startskript stoppt dann die Engine. */
static gboolean fenster_schliessen(GtkWidget *widget, GdkEvent *ereignis, gpointer daten)
{
    (void)ereignis;
    (void)daten;
    if (tray_angemeldet) {
        gtk_widget_hide(widget);
        g_print("crosspoint-fenster: Fenster in den Tray gelegt\n");
        return TRUE;
    }
    gtk_main_quit();
    return TRUE;
}

static void fenster_zu(GtkWidget *widget, gpointer daten)
{
    (void)widget;
    (void)daten;
    gtk_main_quit();
}

/* Adresse aus CROSSPOINT_URL uebernehmen (Host/Port fuer die Warteschleife). */
static void adresse_setzen(void)
{
    const char *aus_umgebung = g_getenv("CROSSPOINT_URL");
    const char *ausgewaehlt = (aus_umgebung && *aus_umgebung) ? aus_umgebung : url;
    GError *fehler = NULL;
    GUri *uri = g_uri_parse(ausgewaehlt, G_URI_FLAGS_NONE, &fehler);
    if (!uri) {
        g_printerr("crosspoint-fenster: %s ist keine gueltige Adresse (%s)\n",
                   ausgewaehlt, fehler ? fehler->message : "?");
        if (fehler) {
            g_error_free(fehler);
        }
        return;
    }
    const char *h = g_uri_get_host(uri);
    int p = g_uri_get_port(uri);
    if (h && *h) {
        g_strlcpy(host, h, sizeof host);
    }
    if (p > 0) {
        port = (guint16)p;
    }
    url = ausgewaehlt;
    g_uri_unref(uri);
}

/* --minimized aus der Kommandozeile nehmen, bevor GTK sie sieht. */
static void eigene_argumente(int *argc, char ***argv)
{
    int ziel = 1;
    for (int i = 1; i < *argc; i++) {
        const char *a = (*argv)[i];
        if (strcmp(a, "--minimized") == 0 || strcmp(a, "-m") == 0) {
            minimiert = TRUE;
            continue;
        }
        (*argv)[ziel++] = (*argv)[i];
    }
    (*argv)[ziel] = NULL;
    *argc = ziel;
}

static void fenster_bauen(void)
{
    gboolean devtools = FALSE;
    const char *dt = g_getenv("CROSSPOINT_DEVTOOLS");
    if (dt && *dt && strcmp(dt, "0") != 0) {
        devtools = TRUE;
    }

    fenster = gtk_window_new(GTK_WINDOW_TOPLEVEL);
    gtk_window_set_title(GTK_WINDOW(fenster), TITEL);
    gtk_window_set_default_size(GTK_WINDOW(fenster), BREITE, HOEHE);
    gtk_window_set_icon_name(GTK_WINDOW(fenster), "de.weppelmann.Crosspoint");
    g_set_application_name(TITEL);
    g_set_prgname(TITEL);

    /* Hintergrund, bevor die Seite zeichnet: das Dunkel der Oberflaeche
     * (#060A12), damit beim Oeffnen kein weisses Bild aufblitzt. */
    GdkRGBA hintergrund;
    gdk_rgba_parse(&hintergrund, "#060A12");

    view = webkit_web_view_new();
    WebKitSettings *einstellungen = webkit_settings_new_with_settings(
        "enable-developer-extras", devtools, "enable-write-console-messages-to-stdout",
        devtools, "javascript-can-open-windows-automatically", FALSE, NULL);
    webkit_web_view_set_settings(WEBKIT_WEB_VIEW(view), einstellungen);
    g_object_unref(einstellungen);
    webkit_web_view_set_background_color(WEBKIT_WEB_VIEW(view), &hintergrund);
    /* Zoom 1.0: die Seite rechnet mit festen Pixelgroessen (1760x980). */
    webkit_web_view_set_zoom_level(WEBKIT_WEB_VIEW(view), 1.0);

    gtk_container_add(GTK_CONTAINER(fenster), view);

    g_signal_connect(view, "load-failed", G_CALLBACK(lade_fehler), NULL);
    g_signal_connect(view, "load-changed", G_CALLBACK(lade_status), NULL);
    g_signal_connect(view, "context-menu", G_CALLBACK(kein_kontextmenue), NULL);
    g_signal_connect(fenster, "delete-event", G_CALLBACK(fenster_schliessen), NULL);
    g_signal_connect(fenster, "destroy", G_CALLBACK(fenster_zu), NULL);
}

/* Haelt die Engine am Leben, auch wenn nur das Tray-Icon da ist: ohne Anfrage
 * beendet sie sich nach ihrer Leerlaufzeit. */
static gboolean wachhalter(gpointer daten)
{
    (void)daten;
    char *antwort = http_anfrage("GET", "/api/state", NULL, NULL);
    g_free(antwort);
    return G_SOURCE_CONTINUE;
}

int main(int argc, char **argv)
{
    eigene_argumente(&argc, &argv);
    gtk_init(&argc, &argv);
    adresse_setzen();

    fenster_bauen();
    gboolean tray_ok = tray_starten();

    if (!minimiert) {
        gtk_widget_show_all(fenster);
    } else if (!tray_ok) {
        /* Ohne Tray waere ein unsichtbares Programm nicht mehr erreichbar. */
        g_printerr("crosspoint-fenster: kein Tray verfuegbar - Fenster wird gezeigt\n");
        gtk_widget_show_all(fenster);
    } else {
        g_print("crosspoint-fenster: minimiert gestartet (nur Tray)\n");
    }

    g_timeout_add_seconds(WACHHALTER_S, wachhalter, NULL);

    /* Warten, bis die Engine antwortet - dabei bleiben die Fensterereignisse
     * in Gang, damit das Fenster schon sichtbar ist (dunkler Hintergrund). */
    int warte_sekunden = 60;
    const char *w = g_getenv("CROSSPOINT_WARTE_S");
    if (w && *w) {
        int v = atoi(w);
        if (v > 0) {
            warte_sekunden = v;
        }
    }
    int schritte = warte_sekunden * 4;
    for (int i = 0; i < schritte; i++) {
        if (tcp_erreichbar()) {
            break;
        }
        g_usleep(250 * 1000);
        while (gtk_events_pending()) {
            gtk_main_iteration_do(FALSE);
        }
    }
    seite_laden();

    gtk_main();

    g_print("crosspoint-fenster: Fenster geschlossen\n");
    g_free(busname);
    return 0;
}
