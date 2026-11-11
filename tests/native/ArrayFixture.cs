// Test-only native API fixture: SQL itself cannot populate Firebird arrays.
using System;
using System.Runtime.InteropServices;
using System.Text;

public static class FirebirdArrayFixture
{
    [DllImport("kernel32", CharSet = CharSet.Unicode, SetLastError = true)]
    private static extern IntPtr LoadLibrary(string path);
    [DllImport("fbclient", CallingConvention = CallingConvention.Cdecl)]
    private static extern IntPtr isc_attach_database(IntPtr[] status, short length, byte[] name, ref IntPtr db, short dpbLength, byte[] dpb);
    [DllImport("fbclient", CallingConvention = CallingConvention.Cdecl)]
    private static extern IntPtr isc_start_transaction(IntPtr[] status, ref IntPtr transaction, short count, ref IntPtr db, short tpbLength, byte[] tpb);
    [DllImport("fbclient", CallingConvention = CallingConvention.Cdecl)]
    private static extern IntPtr isc_array_lookup_bounds(IntPtr[] status, ref IntPtr db, ref IntPtr transaction, string relation, string field, byte[] descriptor);
    [DllImport("fbclient", CallingConvention = CallingConvention.Cdecl)]
    private static extern IntPtr isc_array_put_slice(IntPtr[] status, ref IntPtr db, ref IntPtr transaction, byte[] id, byte[] descriptor, int[] values, ref int length);
    [DllImport("fbclient", CallingConvention = CallingConvention.Cdecl)]
    private static extern IntPtr isc_dsql_execute_immediate(IntPtr[] status, ref IntPtr db, ref IntPtr transaction, ushort length, byte[] sql, ushort dialect, IntPtr sqlda);
    [DllImport("fbclient", CallingConvention = CallingConvention.Cdecl)]
    private static extern IntPtr isc_commit_transaction(IntPtr[] status, ref IntPtr transaction);
    [DllImport("fbclient", CallingConvention = CallingConvention.Cdecl)]
    private static extern IntPtr isc_rollback_transaction(IntPtr[] status, ref IntPtr transaction);
    [DllImport("fbclient", CallingConvention = CallingConvention.Cdecl)]
    private static extern IntPtr isc_detach_database(IntPtr[] status, ref IntPtr db);

    private static void Check(IntPtr result, IntPtr[] status)
    {
        if (result != IntPtr.Zero) throw new Exception("Native array fixture failed, Firebird status " + status[1]);
    }

    public static void Populate(string library, string database, string user, string password)
    {
        if (IntPtr.Size != 8 || LoadLibrary(library) == IntPtr.Zero) throw new Exception("Could not load the test server's 64-bit fbclient");
        byte[] userBytes = Encoding.UTF8.GetBytes(user), passBytes = Encoding.UTF8.GetBytes(password);
        if (userBytes.Length > 255 || passBytes.Length > 255) throw new Exception("Fixture credentials too long");
        byte[] dpb = new byte[7 + userBytes.Length + passBytes.Length];
        int offset = 0;
        dpb[offset++] = 1; dpb[offset++] = 77; dpb[offset++] = 0;
        dpb[offset++] = 28; dpb[offset++] = (byte)userBytes.Length;
        Array.Copy(userBytes, 0, dpb, offset, userBytes.Length); offset += userBytes.Length;
        dpb[offset++] = 29; dpb[offset++] = (byte)passBytes.Length;
        Array.Copy(passBytes, 0, dpb, offset, passBytes.Length);
        var status = new IntPtr[20];
        IntPtr db = IntPtr.Zero, transaction = IntPtr.Zero;
        IntPtr sqlda = IntPtr.Zero, data = IntPtr.Zero, indicator = IntPtr.Zero;
        try
        {
            byte[] name = Encoding.UTF8.GetBytes(database);
            Check(isc_attach_database(status, (short)name.Length, name, ref db, (short)dpb.Length, dpb), status);
            byte[] tpb = {3, 9, 2, 6}; // version3, write, concurrency, wait
            Check(isc_start_transaction(status, ref transaction, 1, ref db, (short)tpb.Length, tpb), status);
            byte[] descriptor = new byte[136], id = new byte[8];
            Check(isc_array_lookup_bounds(status, ref db, ref transaction, "NATIVE_DATA", "VALUES_ARRAY", descriptor), status);
            int[] values = {int.MinValue, -1, 0, 42, int.MaxValue, 1234567};
            int length = values.Length * 4;
            Check(isc_array_put_slice(status, ref db, ref transaction, id, descriptor, values, ref length), status);
            if (length != 24) throw new Exception("Unexpected array slice length");
            sqlda = Marshal.AllocHGlobal(184); data = Marshal.AllocHGlobal(8); indicator = Marshal.AllocHGlobal(2);
            Marshal.Copy(new byte[184], 0, sqlda, 184);
            Marshal.Copy(id, 0, data, 8); Marshal.WriteInt16(indicator, 0);
            Marshal.WriteInt16(sqlda, 0, 1); Marshal.WriteInt32(sqlda, 12, 184);
            Marshal.WriteInt16(sqlda, 16, 1); Marshal.WriteInt16(sqlda, 18, 1);
            Marshal.WriteInt16(sqlda, 24, 540); Marshal.WriteInt16(sqlda, 30, 8); // SQL_ARRAY
            Marshal.WriteIntPtr(sqlda, 32, data); Marshal.WriteIntPtr(sqlda, 40, indicator);
            byte[] sql = Encoding.ASCII.GetBytes("INSERT INTO NATIVE_DATA(ID, VALUES_ARRAY) VALUES(43, ?)");
            Check(isc_dsql_execute_immediate(status, ref db, ref transaction, (ushort)sql.Length, sql, 3, sqlda), status);
            Check(isc_commit_transaction(status, ref transaction), status);
        }
        finally
        {
            if (transaction != IntPtr.Zero) isc_rollback_transaction(status, ref transaction);
            if (db != IntPtr.Zero) isc_detach_database(status, ref db);
            if (sqlda != IntPtr.Zero) Marshal.FreeHGlobal(sqlda);
            if (data != IntPtr.Zero) Marshal.FreeHGlobal(data);
            if (indicator != IntPtr.Zero) Marshal.FreeHGlobal(indicator);
            Array.Clear(dpb, 0, dpb.Length); Array.Clear(passBytes, 0, passBytes.Length);
        }
    }
}
