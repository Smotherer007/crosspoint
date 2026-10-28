/* crosspoint-fenster – die Crosspoint-Oberflaeche in einem eigenen Fenster.
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
 *   CROSSPOINT_URL        Adresse der Oberflaeche (Vorgabe http://127.0.0.1:8770/)
 *   CROSSPOINT_DEVTOOLS   1 = Entwicklerwerkzeuge (Ctrl+Shift+I) einschalten
 *   CROSSPOINT_WARTE_S    Sekunden, die auf die Engine gewartet wird (Vorgabe 60)
 *
 * Schliessen des Fensters beendet das Programm mit 0; das Startskript
 * (scripts/crosspoint-start.sh) stoppt danach die Engine.
 */
#include <gtk/gtk.h>
#include <webkit2/webkit2.h>

#include <stdlib.h>
#include <string.h>

#define TITEL "Crosspoint"
#define BREITE 1760
#define HOEHE 980
/* Nach so vielen erfolglosen Versuchen gibt das Fenster auf (250 ms je
 * Versuch, also gut eine Minute). */
#define MAX_VERSUCHE 240

static GtkWidget *view;
static const char *url = "http://127.0.0.1:8770/";
static char host[256] = "127.0.0.1";
static guint16 port = 8770;
static int versuche;

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
        gtk_main_quit();
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
        gtk_main_quit();
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

int main(int argc, char **argv)
{
    gtk_init(&argc, &argv);
    adresse_setzen();

    gboolean devtools = FALSE;
    const char *dt = g_getenv("CROSSPOINT_DEVTOOLS");
    if (dt && *dt && strcmp(dt, "0") != 0) {
        devtools = TRUE;
    }

    GtkWidget *fenster = gtk_window_new(GTK_WINDOW_TOPLEVEL);
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
    g_signal_connect(fenster, "destroy", G_CALLBACK(fenster_zu), NULL);

    gtk_widget_show_all(fenster);

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
    return 0;
}
